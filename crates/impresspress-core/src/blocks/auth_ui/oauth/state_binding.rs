//! Binds an OAuth round-trip to the browser that started it.
//!
//! The `state` parameter the provider echoes back proves only that *some*
//! flow this deployment started is being completed — not that the browser
//! completing it is the browser that started it. Without a second half, an
//! attacker can run the authorize step in their own browser, keep the
//! resulting `code`/`state` pair, and hand the victim the callback URL: the
//! victim's browser silently signs in to the attacker's account (login CSRF),
//! and anything the victim then saves lands in an account the attacker can
//! read.
//!
//! So `start.rs` also sets a cookie carrying the SHA-256 of the `state_id`,
//! and `callback.rs` refuses any callback whose `state` does not hash to the
//! cookie it arrives with. The attacker's cookie stays in the attacker's
//! browser, so the forged callback has nothing to match.
//!
//! Details that matter:
//!
//! * The cookie holds the *hash*, never the `state_id` itself. The `state_id`
//!   travels in URLs (the authorize redirect, the callback query string) and
//!   so leaks into `Referer` headers, proxy logs and browser history; the
//!   value that binds the flow does not.
//! * One cookie **per flow**, named after the first bytes of that hash. A
//!   single fixed name would mean a single pending flow per browser: a second
//!   tab, or "Google, then GitHub on second thoughts", would overwrite the
//!   first flow's binding and its callback would be refused.
//! * `__Host-` prefix wherever `Secure` is emitted. Without it a sibling
//!   subdomain — or anything that can write cookies for the registrable
//!   domain — can plant a binding for a flow it started and restore exactly
//!   the CSRF this module exists to close; `__Host-` cookies can only be set
//!   by the exact host, over HTTPS, with `Path=/` and no `Domain`. The prefix
//!   is dropped on a development deployment for the same reason `Secure` is:
//!   a browser rejects a `__Host-` cookie that is not `Secure`, so keeping it
//!   on `http://localhost` would leave no binding at all.
//! * `SameSite=Lax`, because the callback arrives as a cross-site top-level
//!   navigation from the provider — `Strict` would withhold the cookie there
//!   and break every sign-in.
//! * `Path=/`, which `__Host-` requires anyway, and which
//!   `IMPRESSPRESS__AUTH_UI__OAUTH_REDIRECT_URI` needs: it is
//!   operator-configurable and need not sit under the start endpoint's path.
//!
//! The cookie is first-party because `start.rs` answers a top-level
//! navigation with a 302 to the provider: the browser is *at* this origin
//! when the cookie is set, in the tab or popup the user is signing in with.
//! An earlier shape — a JSON endpoint the page fetched — put that write in a
//! third-party context for any caller served from another origin, which
//! Safari blocks outright and Firefox partitions, so the callback would never
//! find the binding.

use wafer_run::{context::Context, Message, WaferError};

use crate::{blocks::auth::helpers::cookie_secure_attribute, util::sha256_hex};

/// Cookie-name stem. The per-flow discriminator and, on a secure deployment,
/// the `__Host-` prefix are appended/prepended by [`cookie_name`].
const COOKIE_STEM: &str = "oauth_state";

/// Number of hex characters of the binding hash that name the cookie. 8 hex
/// characters = 32 bits: enough that two flows a user has open at once do not
/// collide, while the full 256-bit hash stays in the value, which is what is
/// actually compared.
const NAME_DISCRIMINATOR_LEN: usize = 8;

/// The value stored in the cookie for `state_id`: hex SHA-256, so the cookie
/// never repeats a value that also travels in a URL.
fn binding_hash(state_id: &str) -> String {
    sha256_hex(state_id.as_bytes())
}

/// The cookie name for one flow. `secure_attribute` is the output of
/// [`cookie_secure_attribute`]; an empty one means this deployment serves
/// plain HTTP, where a `__Host-` cookie would be rejected.
fn cookie_name(hash: &str, secure_attribute: &str) -> String {
    let prefix = if secure_attribute.is_empty() {
        ""
    } else {
        "__Host-"
    };
    format!("{prefix}{COOKIE_STEM}_{}", &hash[..NAME_DISCRIMINATOR_LEN])
}

/// `Set-Cookie` value binding `state_id` to this browser for `max_age_secs`,
/// which the caller keeps equal to the PKCE state's own TTL so the two halves
/// of a flow expire together.
pub(super) async fn issue(
    ctx: &dyn Context,
    state_id: &str,
    max_age_secs: i64,
) -> Result<String, WaferError> {
    let secure = cookie_secure_attribute(ctx).await?;
    let hash = binding_hash(state_id);
    Ok(format!(
        "{}={hash}; HttpOnly; Path=/; SameSite=Lax; Max-Age={max_age_secs}{secure}",
        cookie_name(&hash, secure),
    ))
}

/// `Set-Cookie` value that removes this flow's binding cookie. Emitted on
/// every answer a callback gives once it has matched a binding — the flow is
/// over whether it succeeded or was refused, and a binding that outlives its
/// single-use state is a cookie the browser keeps offering for nothing.
pub(super) async fn clear(ctx: &dyn Context, state_id: &str) -> Result<String, WaferError> {
    let secure = cookie_secure_attribute(ctx).await?;
    let hash = binding_hash(state_id);
    Ok(format!(
        "{}=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0{secure}",
        cookie_name(&hash, secure),
    ))
}

/// Whether `msg` carries this flow's binding cookie.
///
/// A missing cookie is a mismatch: the whole point is that a browser which
/// never ran the start endpoint cannot complete the flow.
///
/// The comparison is a plain `==`, not a constant-time one. Both operands are
/// derived from `state_id`, which the caller supplied in the URL and the
/// provider echoes in the clear; there is no secret here whose bytes a timing
/// side channel could recover, only a value the attacker already has.
pub(super) async fn matches(
    ctx: &dyn Context,
    msg: &Message,
    state_id: &str,
) -> Result<bool, WaferError> {
    let secure = cookie_secure_attribute(ctx).await?;
    let hash = binding_hash(state_id);
    let presented = msg.cookie(&cookie_name(&hash, secure));
    Ok(!presented.is_empty() && presented == hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config_vars::ENVIRONMENT_KEY, test_support::TestContext};

    /// The `Cookie` header a browser sends for a `Set-Cookie` value: the
    /// name/value pair without the attributes.
    pub(super) fn cookie_header_for(set_cookie: &str) -> &str {
        set_cookie.split(';').next().unwrap_or("")
    }

    fn msg_with_cookie(header: &str) -> Message {
        let mut msg = Message::new("auth.oauth.callback");
        msg.set_meta("http.header.cookie", header);
        msg
    }

    async fn dev_ctx() -> TestContext {
        TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID)
    }

    /// A deployment that serves HTTPS — `WAFER_RUN_SHARED__ENVIRONMENT` is
    /// anything but `development`.
    async fn prod_ctx() -> TestContext {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.set_config(ENVIRONMENT_KEY, "production");
        ctx
    }

    #[tokio::test]
    async fn matches_only_the_hash_of_the_state_id() {
        let ctx = dev_ctx().await;
        let state = "abc123";
        let issued = issue(&ctx, state, 600).await.expect("binding cookie");
        assert!(
            matches(&ctx, &msg_with_cookie(cookie_header_for(&issued)), state)
                .await
                .expect("binding read")
        );

        // The raw state id is NOT the cookie value — a caller that echoed the
        // URL parameter into the cookie would not satisfy the binding.
        let name = cookie_name(&binding_hash(state), "");
        assert!(
            !matches(&ctx, &msg_with_cookie(&format!("{name}={state}")), state)
                .await
                .expect("binding read")
        );

        let other = issue(&ctx, "some-other-state", 600)
            .await
            .expect("binding cookie");
        assert!(
            !matches(&ctx, &msg_with_cookie(cookie_header_for(&other)), state)
                .await
                .expect("binding read")
        );
    }

    #[tokio::test]
    async fn a_message_without_the_cookie_never_matches() {
        let ctx = dev_ctx().await;
        assert!(
            !matches(&ctx, &Message::new("auth.oauth.callback"), "abc123")
                .await
                .expect("binding read")
        );
        let name = cookie_name(&binding_hash("abc123"), "");
        assert!(
            !matches(&ctx, &msg_with_cookie(&format!("{name}=")), "abc123")
                .await
                .expect("binding read")
        );
    }

    /// Two flows in flight at once — a second tab, or a change of provider —
    /// each keep their own binding, and each callback still matches.
    #[tokio::test]
    async fn two_pending_flows_do_not_evict_each_other() {
        let ctx = dev_ctx().await;
        let first = issue(&ctx, "state-one", 600).await.expect("binding cookie");
        let second = issue(&ctx, "state-two", 600).await.expect("binding cookie");
        assert_ne!(
            cookie_header_for(&first).split('=').next(),
            cookie_header_for(&second).split('=').next(),
            "each flow must get its own cookie name, or the second start \
             silently cancels the first"
        );

        // The browser now holds both, as one `Cookie` header.
        let both = format!(
            "{}; {}",
            cookie_header_for(&first),
            cookie_header_for(&second)
        );
        assert!(matches(&ctx, &msg_with_cookie(&both), "state-one")
            .await
            .expect("binding read"));
        assert!(matches(&ctx, &msg_with_cookie(&both), "state-two")
            .await
            .expect("binding read"));
    }

    #[tokio::test]
    async fn issued_cookie_carries_the_hash_and_the_lax_http_only_attributes() {
        let ctx = dev_ctx().await;
        let cookie = issue(&ctx, "abc123", 600).await.expect("binding cookie");
        assert!(
            cookie.contains(&format!("={}", binding_hash("abc123"))),
            "cookie must carry the hash, not the state id: {cookie}"
        );
        assert!(
            !cookie.contains("=abc123"),
            "state id must not be in the cookie: {cookie}"
        );
        assert!(cookie.contains("; HttpOnly"), "{cookie}");
        assert!(cookie.contains("; SameSite=Lax"), "{cookie}");
        assert!(cookie.contains("; Path=/"), "{cookie}");
        assert!(cookie.contains("; Max-Age=600"), "{cookie}");
    }

    /// On HTTPS the cookie is `__Host-` prefixed, which is what stops a
    /// sibling subdomain planting a binding for the parent domain. On plain
    /// HTTP it is not, because a browser rejects a `__Host-` cookie without
    /// `Secure` and the binding would simply not exist.
    #[tokio::test]
    async fn the_host_prefix_tracks_the_secure_attribute() {
        let prod = prod_ctx().await;
        let secure_cookie = issue(&prod, "abc123", 600).await.expect("binding cookie");
        assert!(
            secure_cookie.starts_with("__Host-"),
            "a Secure cookie must carry the __Host- prefix: {secure_cookie}"
        );
        assert!(secure_cookie.contains("; Secure"), "{secure_cookie}");
        assert!(
            matches(
                &prod,
                &msg_with_cookie(cookie_header_for(&secure_cookie)),
                "abc123"
            )
            .await
            .expect("binding read"),
            "the callback must look for the same name the start endpoint set"
        );

        let dev = dev_ctx().await;
        let dev_cookie = issue(&dev, "abc123", 600).await.expect("binding cookie");
        assert!(
            !dev_cookie.contains("__Host-"),
            "a non-Secure cookie must not claim __Host-, browsers drop it: {dev_cookie}"
        );
        assert!(!dev_cookie.contains("; Secure"), "{dev_cookie}");
    }

    #[tokio::test]
    async fn clear_expires_the_cookie_the_flow_was_issued() {
        let ctx = dev_ctx().await;
        let issued = issue(&ctx, "abc123", 600).await.expect("binding cookie");
        let cleared = clear(&ctx, "abc123").await.expect("binding cookie");
        let issued_name = cookie_header_for(&issued).split('=').next().unwrap();
        assert!(
            cleared.starts_with(&format!("{issued_name}=;")),
            "clear must name the cookie that was issued: {cleared} vs {issued}"
        );
        assert!(cleared.contains("; Max-Age=0"), "{cleared}");
        assert!(cleared.contains("; Path=/"), "{cleared}");
    }
}
