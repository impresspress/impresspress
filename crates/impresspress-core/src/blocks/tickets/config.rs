//! Configuration and fail-closed public-submission readiness.

use serde::Serialize;
use wafer_core::clients::config;
use wafer_run::{context::Context, ConfigVar, InputType};

use super::repo;

pub const PUBLIC_ENABLED: &str = "IMPRESSPRESS__TICKETS__PUBLIC_SUBMISSIONS_ENABLED";
pub const BACK_URL: &str = "IMPRESSPRESS__TICKETS__PUBLIC_BACK_URL";
pub const SUPPORT_EMAIL: &str = "IMPRESSPRESS__TICKETS__SUPPORT_EMAIL";
pub const TURNSTILE_SITE_KEY: &str = "IMPRESSPRESS__TICKETS__TURNSTILE_SITE_KEY";
pub const TURNSTILE_SECRET_KEY: &str = "IMPRESSPRESS__TICKETS__TURNSTILE_SECRET_KEY";
pub const IDENTITY_SECRET: &str = "IMPRESSPRESS__TICKETS__IDENTITY_SECRET";
pub const IDENTITY_MAX: &str = "IMPRESSPRESS__TICKETS__IDENTITY_MAX";
pub const IDENTITY_WINDOW: &str = "IMPRESSPRESS__TICKETS__IDENTITY_WINDOW_SECS";
pub const GLOBAL_MAX: &str = "IMPRESSPRESS__TICKETS__GLOBAL_MAX";
pub const GLOBAL_WINDOW: &str = "IMPRESSPRESS__TICKETS__GLOBAL_WINDOW_SECS";
pub const FORM_TTL: &str = "IMPRESSPRESS__TICKETS__FORM_TTL_SECS";
pub const RETENTION_SPAM: &str = "IMPRESSPRESS__TICKETS__RETENTION_SPAM_DAYS";
pub const RETENTION_REJECTED: &str = "IMPRESSPRESS__TICKETS__RETENTION_REJECTED_DAYS";
pub const RETENTION_RESOLVED: &str = "IMPRESSPRESS__TICKETS__RETENTION_RESOLVED_DAYS";

pub fn config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            PUBLIC_ENABLED,
            "Allow protected public ticket submissions after readiness checks pass",
            "false",
        )
        .name("Public submissions enabled"),
        ConfigVar::new(
            BACK_URL,
            "Safe same-site back link for the public form",
            "/",
        )
        .name("Public back URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            SUPPORT_EMAIL,
            "Contact address shown on the public form as an alternative to submitting a report; leave empty to show none",
            "",
        )
        .name("Support email")
        .optional(),
        ConfigVar::new(
            TURNSTILE_SITE_KEY,
            "Cloudflare Turnstile public widget site key",
            "",
        )
        .name("Turnstile site key")
        .optional(),
        ConfigVar::new(
            TURNSTILE_SECRET_KEY,
            "Cloudflare Turnstile server-side Siteverify secret",
            "",
        )
        .name("Turnstile secret")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            IDENTITY_SECRET,
            "Independent high-entropy secret for rotating abuse and form-token digests",
            "",
        )
        .name("Identity secret")
        .input_type(InputType::Password)
        .optional(),
        number(IDENTITY_MAX, "Reports per identity", "3"),
        number(IDENTITY_WINDOW, "Identity limit window (seconds)", "3600"),
        number(GLOBAL_MAX, "Global reports per window", "100"),
        number(GLOBAL_WINDOW, "Global limit window (seconds)", "3600"),
        number(FORM_TTL, "Public form lifetime (seconds)", "7200"),
        number(RETENTION_SPAM, "Spam retention (days)", "30"),
        number(
            RETENTION_REJECTED,
            "Rejected/duplicate retention (days)",
            "180",
        ),
        number(RETENTION_RESOLVED, "Resolved retention (days)", "365"),
    ]
}

fn number(key: &str, name: &str, default: &str) -> ConfigVar {
    ConfigVar::new(key, name, default)
        .name(name)
        .input_type(InputType::Number)
}

// Every field is a boolean "is this configured", never a configured value:
// the two secrets behind `turnstile_secret_configured` and
// `identity_secret_configured` are only ever tested for emptiness here, so
// this shape cannot carry one. Reaching it needs admin auth in any case — the
// public form renders `reasons` only under `debug_assertions`.
/// Whether protected public reporting can currently accept a submission.
///
/// Public reporting fails closed: every check below must pass, and `reasons`
/// names the ones that did not.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SecurityReadiness {
    /// Whether all of the checks below passed.
    pub ready: bool,
    /// Whether the tickets block itself is enabled.
    pub block_enabled: bool,
    /// Whether the operator has turned public submissions on.
    pub public_enabled: bool,
    /// Whether a Turnstile widget site key is configured.
    pub site_key_configured: bool,
    /// Whether a Turnstile Siteverify secret is configured.
    pub turnstile_secret_configured: bool,
    /// Whether the abuse-digest identity secret is configured.
    pub identity_secret_configured: bool,
    /// Whether both submission rate limits have a positive cap and window.
    pub positive_limits: bool,
    /// Whether at least one active, publicly visible ticket type exists.
    pub has_public_type: bool,
    /// Human-readable reason for each failed check, in check order. Empty when
    /// `ready`.
    pub reasons: Vec<String>,
}

impl SecurityReadiness {
    /// A failed config read is returned: readiness is what lets the public
    /// form accept anonymous submissions, so it is never guessed.
    pub async fn load(ctx: &dyn Context) -> Result<Self, wafer_run::WaferError> {
        // The boot snapshot, deliberately, where the sidebar and the portal's
        // feature list read `routing::gate_from_request` instead.
        //
        // This value can go stale against the router after an admin toggle,
        // and that staleness is unobservable: every surface that reads a
        // `SecurityReadiness` — the admin readiness panel, the public submit
        // form — is itself a `/b/tickets` route, so a disabled tickets block
        // means the router refuses the page before the flag can be rendered.
        // The one caller that is not a route (`maintenance`, a scheduled
        // sweep) has no routed message to read a gate from at all. Threading
        // one through it to correct a discrepancy nothing can see would buy
        // nothing.
        let block_enabled = ctx
            .config_get(crate::features::BLOCK_SETTINGS_CONFIG_KEY)
            .map(|value| {
                crate::features::BlockSettings::state_for(value, "impresspress/tickets").enabled
            })
            .unwrap_or(true);
        let public_enabled = crate::config_vars::get_bool(ctx, PUBLIC_ENABLED, false).await?;
        let site_key_configured = !config::get_default(ctx, TURNSTILE_SITE_KEY, "")
            .await?
            .trim()
            .is_empty();
        let turnstile_secret_configured = !config::get_default(ctx, TURNSTILE_SECRET_KEY, "")
            .await?
            .trim()
            .is_empty();
        let identity_secret_configured = !config::get_default(ctx, IDENTITY_SECRET, "")
            .await?
            .trim()
            .is_empty();
        let identity_max = positive(ctx, IDENTITY_MAX, 3).await?;
        let identity_window = positive(ctx, IDENTITY_WINDOW, 3_600).await?;
        let global_max = positive(ctx, GLOBAL_MAX, 100).await?;
        let global_window = positive(ctx, GLOBAL_WINDOW, 3_600).await?;
        let positive_limits = identity_max && identity_window && global_max && global_window;
        let has_public_type = repo::count_public_types(ctx)
            .await
            .is_ok_and(|count| count > 0);

        let checks = [
            (block_enabled, "tickets block is disabled"),
            (public_enabled, "public submissions are disabled"),
            (site_key_configured, "Turnstile site key is missing"),
            (
                turnstile_secret_configured,
                "Turnstile server secret is missing",
            ),
            (identity_secret_configured, "identity secret is missing"),
            (positive_limits, "rate limits must be positive"),
            (has_public_type, "no active public ticket type exists"),
        ];
        let reasons = checks
            .into_iter()
            .filter(|(ok, _)| !ok)
            .map(|(_, reason)| reason.to_string())
            .collect::<Vec<_>>();
        Ok(Self {
            ready: reasons.is_empty(),
            block_enabled,
            public_enabled,
            site_key_configured,
            turnstile_secret_configured,
            identity_secret_configured,
            positive_limits,
            has_public_type,
            reasons,
        })
    }
}

/// `key` as a `u32`: `default` when unset or unparseable, the read's error
/// when it fails.
pub async fn u32_value(
    ctx: &dyn Context,
    key: &str,
    default: u32,
) -> Result<u32, wafer_run::WaferError> {
    Ok(config::get_default(ctx, key, &default.to_string())
        .await?
        .parse()
        .unwrap_or(default))
}

/// `key` as a `u64`: `default` when unset or unparseable, the read's error
/// when it fails.
pub async fn u64_value(
    ctx: &dyn Context,
    key: &str,
    default: u64,
) -> Result<u64, wafer_run::WaferError> {
    Ok(config::get_default(ctx, key, &default.to_string())
        .await?
        .parse()
        .unwrap_or(default))
}

async fn positive(
    ctx: &dyn Context,
    key: &str,
    default: u64,
) -> Result<bool, wafer_run::WaferError> {
    Ok(u64_value(ctx, key, default).await? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_declared_as_passwords() {
        let vars = config_vars();
        for key in [TURNSTILE_SECRET_KEY, IDENTITY_SECRET] {
            let var = vars.iter().find(|var| var.key == key).expect("declared");
            assert_eq!(var.input_type, InputType::Password);
        }
    }
}
