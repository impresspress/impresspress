//! POST /b/auth/api/change-password — relocated from auth/login.rs in Task 5.

use maud::html;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::{
        auth::{
            check_password, end_credentials_after_password_change, hash_new_password,
            repo::{local_credentials, users},
            PasswordCheck,
        },
        auth_ui::contracts::MessageResponse,
        crud,
        errors::{error_response, ErrorCode},
    },
    http::{err_bad_request, err_internal, err_not_found, ok_json, ResponseBuilder},
    ui::{
        components::{alert_message, Action, ActionTarget, AlertVariant},
        is_htmx,
    },
    util::parse_body_value,
};

/// The `id` of the change-password form on the portal's Security page
/// (`blocks/userportal/pages/security.rs`), the one form that posts here with
/// htmx. A successful change replaces it — see [`changed_response`].
pub const FORM_ID: &str = "change-password-form";

/// The `id` of that form's result slot: its `hx-target`, where a refusal is
/// swapped in beside the fields the caller is about to correct.
pub const RESULT_ID: &str = "change-password-result";

/// The sentence a confirmation that differs from the new password is refused
/// with.
const MISMATCH: &str = "New passwords do not match.";

/// The answer to a change that happened, in the shape its caller can use.
///
/// The user portal's Security page posts this form with htmx and swaps the
/// response into [`RESULT_ID`], so a JSON body would be rendered into the page
/// as text. Programmatic callers post JSON and parse the answer as JSON, so
/// they keep the [`MessageResponse`] envelope. Same split as
/// [`super::api_keys::handle_create`].
///
/// The htmx answer replaces the whole form ([`FORM_ID`], `HX-Retarget` +
/// `HX-Reswap: outerHTML`) rather than landing in the result slot. The change
/// ends every device and API key and bumps the user's `auth_version`
/// ([`end_credentials_after_password_change`]), which retires the access token
/// the page itself is holding (see [`crate::crypto::verify_access_token`]) — so the
/// form, still filled in with the old and new password, has nothing left to
/// submit. What replaces it says so and offers the way back in, landing on
/// the page they were on.
fn changed_response(msg: &Message) -> OutputStream {
    if is_htmx(msg) {
        let markup = html! {
            div id=(FORM_ID) .change-password-done {
                (alert_message(
                    AlertVariant::Success,
                    "Password changed. You have been signed out everywhere and your API keys have been revoked — sign in again with your new password.",
                ))
                // `autofocus`: htmx focuses it once swapped in, so focus is
                // not left on the submit button the swap just removed. The
                // primary: it replaces the form's own.
                (Action::primary(
                    "Sign in again",
                    ActionTarget::Link("/b/auth/login?redirect=%2Fb%2Fuserportal%2Fsecurity"),
                )
                .autofocus()
                .render())
            }
        };
        return ResponseBuilder::new()
            .set_header("HX-Retarget", &format!("#{FORM_ID}"))
            .set_header("HX-Reswap", "outerHTML")
            .body(
                markup.into_string().into_bytes(),
                "text/html; charset=utf-8",
            );
    }
    ok_json(&MessageResponse {
        message: "Password changed successfully".to_string(),
    })
}

/// A refusal the caller can act on, in the shape that caller can read.
///
/// htmx does not swap a non-2xx response, so an error terminal reaches an
/// htmx caller only as a toast — `ui/assets/chrome.js`'s `htmx:responseError`
/// listener — and leaves [`RESULT_ID`], the slot the form declares for
/// exactly this answer, empty. The sibling handler on that same page,
/// [`crate::blocks::userportal::pages::security::handle_unlink`], already
/// answers its refusal as 200 markup carrying the reason, so this one does
/// too: one page, one convention, and the sentence naming a wrong password
/// stays on screen instead of expiring with a four-second toast. It is the
/// shared [`alert_message`] (`role="alert"`), so swapping it in announces it.
///
/// Only refusals travel this way. A read that could not run or a write that
/// did not land is not a sentence the caller can act on, and stays an error
/// terminal so the status is honest — [`crud::db_error_internal`]'s 403 for a
/// WRAP denial, 429 for a quota, and otherwise the 500 whose correlation id
/// reaches the logs — the same split `handle_unlink` makes.
///
/// JSON callers are untouched: they read the status, so they keep the
/// refusal codes [`error_response`] maps (`401` for a wrong current password,
/// `400` for a password the policy declines or a confirmation that differs).
///
/// The htmx answer also names the field the refusal is about, as an
/// `HX-Trigger: {"focusField": {"id": …}}` that `ui/assets/chrome.js` answers
/// by focusing that field and marking it `aria-invalid` — so the caller lands
/// on what to retype rather than on the submit button. The ids are the
/// form's field names, which are this endpoint's own body fields.
fn refused(msg: &Message, field: Field, code: ErrorCode, reason: &str) -> OutputStream {
    if is_htmx(msg) {
        let trigger = serde_json::json!({ "focusField": { "id": field.name() } });
        return ResponseBuilder::new()
            .set_header("HX-Trigger", &trigger.to_string())
            .body(
                alert_message(AlertVariant::Error, reason)
                    .into_string()
                    .into_bytes(),
                "text/html; charset=utf-8",
            );
    }
    error_response(code, reason)
}

/// The body field — and the Security form's field of that name and id — a
/// refusal is about.
#[derive(Clone, Copy)]
enum Field {
    Current,
    New,
    Confirm,
}

impl Field {
    fn name(self) -> &'static str {
        match self {
            Field::Current => "current_password",
            Field::New => "new_password",
            Field::Confirm => "confirm_password",
        }
    }
}

pub async fn handle(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let user_id = msg.user_id();
    if user_id.is_empty() {
        return crate::http::err_unauthenticated("Not authenticated");
    }

    #[derive(serde::Deserialize)]
    struct ChangePwReq {
        current_password: String,
        new_password: String,
        /// The new password typed a second time. The Security form always
        /// sends it, and a mismatch is refused here rather than by a script
        /// on the page, so the form needs none. Optional for programmatic
        /// callers, which have no typo to catch.
        #[serde(default)]
        confirm_password: Option<String>,
    }
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    // Two callers, two wire formats: the portal's Security page is an htmx
    // form, so it sends `application/x-www-form-urlencoded`, while
    // programmatic clients send JSON.
    // `parse_body_value` reads either, as `api_keys::handle_create` does for
    // the admin block's key form.
    let parsed = match parse_body_value(&raw) {
        Ok(value) => value,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    let body: ChangePwReq = match serde_json::from_value(parsed) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    if body
        .confirm_password
        .as_deref()
        .is_some_and(|confirm| confirm != body.new_password)
    {
        return refused(msg, Field::Confirm, ErrorCode::InvalidInput, MISMATCH);
    }

    match super::password_policy::validate_new_password(ctx, &body.new_password).await {
        Ok(Ok(())) => {}
        Ok(Err((code, reason))) => return refused(msg, Field::New, code, &reason),
        Err(response) => return response,
    }

    // Verify user exists. The credential lookup four lines below has always
    // separated "no row" from "the read failed"; this probe folded both into
    // a 404, so an outage told a signed-in caller their account was gone and
    // left the password unchanged.
    match users::find_by_id(ctx, user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return err_not_found("User not found"),
        Err(e) => return crud::db_error_internal(e, "Could not load the signed-in user"),
    };

    // Fetch existing credential row — must have one to change password.
    let cred = match local_credentials::find_by_user_id(ctx, user_id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return refused(
                msg,
                Field::Current,
                ErrorCode::InvalidCredentials,
                "No password set for this account",
            )
        }
        Err(e) => return crud::db_error_internal(e, "Credential lookup failed"),
    };

    // Only a wrong password is "incorrect". A stored hash the crypto service
    // cannot check (logged with the user id by `check_password`) is this
    // deployment's fault, not the caller's: the caller is already signed in
    // as this account, so there is nothing to hide from them, and telling
    // them their correct password is wrong would send them round in circles.
    // A comparison that could not run is the answer `credential_check_failed`
    // classifies: 403/429, 499/504 for a cancelled or late request, else 503.
    match check_password(ctx, user_id, &body.current_password, &cred.password_hash).await {
        Ok(PasswordCheck::Matches) => {}
        Ok(PasswordCheck::DoesNotMatch) => {
            return refused(
                msg,
                Field::Current,
                ErrorCode::InvalidCredentials,
                "Current password is incorrect",
            );
        }
        Ok(PasswordCheck::Unverifiable(e)) => {
            return err_internal("Stored password hash could not be checked", e);
        }
        Err(e) => return OutputStream::error(e),
    }

    let new_hash = match hash_new_password(ctx, &body.new_password).await {
        Ok(hash) => hash,
        Err(response) => return response,
    };

    if let Err(e) = local_credentials::update_password(ctx, user_id, &new_hash).await {
        return crud::db_error_internal(e, "Update failed");
    }

    // The credential has changed, so every session and API key the old
    // password opened must end, and a failure to end them must not be
    // answered as success.
    if let Err(e) = end_credentials_after_password_change(ctx, user_id).await {
        return crud::db_error_internal(e, "Password changed but session invalidation failed");
    }
    changed_response(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::auth_ui::api::{login, signup},
        test_support::{
            auth_msg, collect_or_panic, output_http_status, output_is_error, output_json,
            output_status, FailingDbOpContext, FailingServiceOpContext, TestContext,
        },
    };

    /// Sign a user up through the real signup handler and return their id.
    async fn signup_user(ctx: &TestContext, email: &str, password: &str) -> String {
        let body = serde_json::json!({"email": email, "password": password}).to_string();
        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        let out = signup::handle(
            &limiter,
            ctx,
            &msg,
            InputStream::from_bytes(body.into_bytes()),
        )
        .await;
        let json = output_json(out).await;
        json["user"]["id"]
            .as_str()
            .expect("signup response carries user.id")
            .to_string()
    }

    fn body(current: &str, new: &str) -> InputStream {
        InputStream::from_bytes(
            serde_json::json!({"current_password": current, "new_password": new})
                .to_string()
                .into_bytes(),
        )
    }

    /// The bytes the user portal's Security form puts on the wire: htmx
    /// serialises a form as `application/x-www-form-urlencoded`, under the
    /// field names those inputs declare in
    /// `blocks/userportal/pages/security.rs`.
    fn form_body(current: &str, new: &str) -> InputStream {
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("current_password", current)
            .append_pair("new_password", new)
            .finish();
        InputStream::from_bytes(encoded.into_bytes())
    }

    /// The request that form arrives in. htmx sets `HX-Request` on every
    /// request it issues, which is what [`crate::ui::is_htmx`] reads.
    fn htmx_msg(user_id: &str) -> Message {
        let mut msg = auth_msg("update", "/b/auth/api/change-password", user_id);
        msg.set_meta("http.header.hx-request", "true");
        msg
    }

    /// The `HX-Trigger` header of a collected response.
    fn hx_trigger(parts: &wafer_block::http_codec::HttpResponseParts) -> Option<String> {
        parts
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("HX-Trigger"))
            .map(|(_, value)| value.clone())
    }

    fn credentials(email: &str, password: &str) -> InputStream {
        InputStream::from_bytes(
            serde_json::json!({"email": email, "password": password})
                .to_string()
                .into_bytes(),
        )
    }

    /// The portal's Security page is the surface most users reach this
    /// endpoint from, and it posts a form. Parsing the body as JSON only, the
    /// handler answered `400 Invalid body` to every one of those posts and
    /// the password never changed.
    ///
    /// The new password carries a space, a `+` and a `&` — the characters
    /// form encoding gives its own meaning — so a body that decoded loosely
    /// would store a password nobody could then sign in with.
    #[tokio::test]
    async fn the_portal_form_changes_the_password() {
        const OLD: &str = "original-horse-battery1";
        const NEW: &str = "corr&ct horse+battery 100%";

        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "erin@example.com", OLD).await;

        let out = handle(&ctx, &htmx_msg(&user_id), form_body(OLD, NEW)).await;
        assert_eq!(output_status(out).await, 200, "the form post must succeed");

        let signed_in = output_json(
            login::handle(
                &ctx,
                &crate::test_support::json_post_msg("/b/auth/api/login"),
                credentials("erin@example.com", NEW),
            )
            .await,
        )
        .await["access_token"]
            .as_str()
            .map(str::to_string);
        assert!(
            signed_in.is_some_and(|t| !t.is_empty()),
            "the new password must authenticate"
        );
        assert!(
            output_is_error(
                login::handle(
                    &ctx,
                    &crate::test_support::json_post_msg("/b/auth/api/login"),
                    credentials("erin@example.com", OLD)
                )
                .await,
                "Unauthenticated"
            )
            .await,
            "the old password must stop authenticating"
        );
    }

    /// The form swaps what comes back into the page, so the success answer to
    /// an htmx caller has to be markup — a JSON envelope would be rendered
    /// into the page as its own text — and it replaces the whole form: the
    /// change ended the session the page holds, so the filled-in form has
    /// nothing left to submit.
    #[tokio::test]
    async fn an_htmx_caller_is_answered_with_markup() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "frank@example.com", "original-horse-battery1").await;

        let out = handle(
            &ctx,
            &htmx_msg(&user_id),
            form_body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        let buf = collect_or_panic(out).await;
        let content_type = buf
            .meta
            .iter()
            .find(|m| m.key == wafer_block::meta::META_RESP_CONTENT_TYPE)
            .map(|m| m.value.clone())
            .unwrap_or_default();
        assert!(
            content_type.starts_with("text/html"),
            "an htmx caller must be answered as HTML, got {content_type:?}"
        );
        let header = |name: &str| {
            buf.meta
                .iter()
                .find(|m| m.key == format!("resp.header.{name}"))
                .map(|m| m.value.clone())
        };
        assert_eq!(
            header("HX-Retarget").as_deref(),
            Some("#change-password-form")
        );
        assert_eq!(header("HX-Reswap").as_deref(), Some("outerHTML"));
        let html = String::from_utf8(buf.body).expect("body was not valid UTF-8");
        assert!(
            html.contains(r#"id="change-password-form""#),
            "the replacement keeps the form's id, got {html:?}"
        );
        assert!(
            html.contains("Password changed.") && html.contains(r#"role="status""#),
            "the fragment must say the change happened, got {html:?}"
        );
        assert!(
            html.contains("/b/auth/login?redirect=%2Fb%2Fuserportal%2Fsecurity"),
            "the session is gone, so it offers the way back in, got {html:?}"
        );
        assert!(
            html.contains(
                r#"<a class="btn btn--primary" autofocus href="/b/auth/login?redirect=%2Fb%2Fuserportal%2Fsecurity">Sign in again</a>"#
            ),
            "focus moves to the way back in, the one primary, got {html:?}"
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(&html).is_err(),
            "a JSON body would be swapped into the page as text, got {html:?}"
        );
    }

    /// A wrong current password is the form's most likely mistake, and until
    /// the body parsed every one of those attempts was reported as a
    /// malformed request instead.
    ///
    /// It comes back as markup, not as an error terminal: htmx does not swap
    /// a non-2xx, so a refusal answered as one leaves the form's result slot ([`RESULT_ID`])
    /// empty and says its piece only in a toast that expires. The sibling
    /// handler on that page (`userportal::pages::security::handle_unlink`)
    /// already answers a refusal as 200 markup.
    #[tokio::test]
    async fn a_form_post_with_the_wrong_current_password_says_so() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "gina@example.com", "original-horse-battery1").await;

        let parts = wafer_block::http_codec::collect_http_response(
            handle(
                &ctx,
                &htmx_msg(&user_id),
                form_body("not-the-current-password", "new-horse-battery-2026"),
            )
            .await,
        )
        .await;

        assert_eq!(
            parts.status, 200,
            "a refusal htmx will not swap never reaches the page"
        );
        assert_eq!(
            hx_trigger(&parts).as_deref(),
            Some(r#"{"focusField":{"id":"current_password"}}"#),
            "focus goes to the field the refusal is about"
        );
        let html = String::from_utf8(parts.body).expect("body was not valid UTF-8");
        assert_eq!(
            html,
            r#"<div class="alert alert--error" role="alert">Current password is incorrect</div>"#,
            "the fragment must name the refusal in the shared alert"
        );

        // The credential is untouched: the account still signs in with what
        // it had.
        assert!(
            output_json(
                login::handle(
                    &ctx,
                    &crate::test_support::json_post_msg("/b/auth/api/login"),
                    credentials("gina@example.com", "original-horse-battery1")
                )
                .await
            )
            .await["access_token"]
                .as_str()
                .is_some(),
            "a refused change must leave the password alone"
        );
    }

    /// Same for a new password the policy refuses: the caller is told which
    /// rule it broke, in the page, not that their request was malformed.
    #[tokio::test]
    async fn a_form_post_the_policy_refuses_names_the_rule() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "hank@example.com", "original-horse-battery1").await;

        let parts = wafer_block::http_codec::collect_http_response(
            handle(
                &ctx,
                &htmx_msg(&user_id),
                form_body("original-horse-battery1", "123456"),
            )
            .await,
        )
        .await;

        assert_eq!(parts.status, 200);
        assert_eq!(
            hx_trigger(&parts).as_deref(),
            Some(r#"{"focusField":{"id":"new_password"}}"#)
        );
        let html = String::from_utf8(parts.body).expect("body was not valid UTF-8");
        assert!(
            html.contains("at least") && html.contains(r#"role="alert""#),
            "the fragment must name the length rule, got {html:?}"
        );
    }

    /// The form's three fields as htmx serialises them.
    fn form_body_confirmed(current: &str, new: &str, confirm: &str) -> InputStream {
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("current_password", current)
            .append_pair("new_password", new)
            .append_pair("confirm_password", confirm)
            .finish();
        InputStream::from_bytes(encoded.into_bytes())
    }

    /// A confirmation that differs from the new password is refused before
    /// anything is checked or written — the server's check, so the form needs
    /// no script of its own — and the password is left as it was.
    #[tokio::test]
    async fn a_confirmation_that_differs_is_refused_and_changes_nothing() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "kim@example.com", "original-horse-battery1").await;

        let parts = wafer_block::http_codec::collect_http_response(
            handle(
                &ctx,
                &htmx_msg(&user_id),
                form_body_confirmed(
                    "original-horse-battery1",
                    "new-horse-battery-2026",
                    "new-horse-battery-2027",
                ),
            )
            .await,
        )
        .await;
        assert_eq!(parts.status, 200);
        assert_eq!(
            hx_trigger(&parts).as_deref(),
            Some(r#"{"focusField":{"id":"confirm_password"}}"#)
        );
        let html = String::from_utf8(parts.body).expect("body was not valid UTF-8");
        assert!(
            html.contains("New passwords do not match.") && html.contains(r#"role="alert""#),
            "{html:?}"
        );
        assert!(
            output_json(
                login::handle(
                    &ctx,
                    &crate::test_support::json_post_msg("/b/auth/api/login"),
                    credentials("kim@example.com", "original-horse-battery1")
                )
                .await
            )
            .await["access_token"]
                .as_str()
                .is_some(),
            "a refused change must leave the password alone"
        );

        // A JSON caller that sends a confirmation gets the same check, as a 400.
        let json = wafer_block::http_codec::collect_http_response(
            handle(
                &ctx,
                &auth_msg("update", "/b/auth/api/change-password", &user_id),
                InputStream::from_bytes(
                    serde_json::json!({
                        "current_password": "original-horse-battery1",
                        "new_password": "new-horse-battery-2026",
                        "confirm_password": "something-else",
                    })
                    .to_string()
                    .into_bytes(),
                ),
            )
            .await,
        )
        .await;
        assert_eq!(json.status, 400);
    }

    /// A matching confirmation is accepted: the form's normal case.
    #[tokio::test]
    async fn a_matching_confirmation_changes_the_password() {
        const NEW: &str = "new-horse-battery-2026";
        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "lee@example.com", "original-horse-battery1").await;

        let out = handle(
            &ctx,
            &htmx_msg(&user_id),
            form_body_confirmed("original-horse-battery1", NEW, NEW),
        )
        .await;
        assert_eq!(output_status(out).await, 200);
        assert!(
            output_json(
                login::handle(
                    &ctx,
                    &crate::test_support::json_post_msg("/b/auth/api/login"),
                    credentials("lee@example.com", NEW)
                )
                .await
            )
            .await["access_token"]
                .as_str()
                .is_some(),
            "the new password must authenticate"
        );
    }

    /// The markup branch is the htmx caller's alone. A JSON client — the
    /// SDK, a script — reads the status, so
    /// its refusals keep the codes and the envelope they have always had.
    #[tokio::test]
    async fn a_json_caller_still_receives_the_refusal_codes() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "ivan@example.com", "original-horse-battery1").await;
        let msg = auth_msg("update", "/b/auth/api/change-password", &user_id);

        let wrong = wafer_block::http_codec::collect_http_response(
            handle(
                &ctx,
                &msg,
                body("not-the-current-password", "new-horse-battery-2026"),
            )
            .await,
        )
        .await;
        assert_eq!(wrong.status, 401);
        let json: serde_json::Value = serde_json::from_slice(&wrong.body).unwrap_or_default();
        assert_eq!(json["message"], "Current password is incorrect");

        let refused_by_policy = wafer_block::http_codec::collect_http_response(
            handle(&ctx, &msg, body("original-horse-battery1", "123456")).await,
        )
        .await;
        assert_eq!(refused_by_policy.status, 400);
        let json: serde_json::Value =
            serde_json::from_slice(&refused_by_policy.body).unwrap_or_default();
        assert!(
            json["message"]
                .as_str()
                .unwrap_or_default()
                .contains("at least"),
            "a JSON caller keeps the sentence too, got {json}"
        );
    }

    /// A refresh token the account holds, from a sign-in through the real
    /// handler.
    async fn sign_in(ctx: &TestContext, email: &str, password: &str) -> String {
        let signed_in = output_json(
            login::handle(
                ctx,
                &crate::test_support::json_post_msg("/b/auth/api/login"),
                credentials(email, password),
            )
            .await,
        )
        .await;
        signed_in["refresh_token"]
            .as_str()
            .unwrap_or_else(|| panic!("the sign-in succeeded: {signed_in}"))
            .to_string()
    }

    fn refresh_with(token: &str) -> InputStream {
        InputStream::from_bytes(
            serde_json::json!({ "refresh_token": token })
                .to_string()
                .into_bytes(),
        )
    }

    /// A refresh-row revocation that fails must not keep the account's
    /// sessions alive, and does not: the session rows were deleted before it
    /// ran, and the `auth_version` bump runs whatever the revocation did. The
    /// refresh token the old password opened — whose row the failed
    /// revocation left live — is refused, so the failure is housekeeping and
    /// the change is answered as done.
    #[tokio::test]
    async fn a_failed_revocation_still_ends_every_session() {
        use crate::blocks::{
            auth::repo::{tokens, users},
            auth_ui::api::refresh,
        };

        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "alice@example.com", "original-horse-battery1").await;
        let old_session = sign_in(&ctx, "alice@example.com", "original-horse-battery1").await;

        // Fail only the refresh-token revocation write
        // (`tokens::revoke_all_for_user` issues a `database.update_where`
        // against the tokens table); the password credential update itself
        // — a *different* `database.update_where` call, against
        // `local_credentials` — still succeeds.
        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.update_where", tokens::TABLE)]);
        let msg = auth_msg("update", "/b/auth/api/change-password", &user_id);
        let out = handle(
            &failing,
            &msg,
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;
        let status = output_http_status(out).await;

        assert!(
            !tokens::find_by_token(&ctx, &old_session)
                .await
                .expect("token lookup")
                .expect("the row is kept")
                .revoked,
            "precondition: the revocation really failed, so the row alone is live"
        );
        assert_eq!(
            users::auth_version(&ctx, &user_id).await.unwrap(),
            1,
            "a failed revocation must not skip the auth_version bump"
        );
        assert!(
            output_is_error(
                refresh::handle(
                    &ctx,
                    &crate::test_support::json_post_msg("/b/auth/api/refresh"),
                    refresh_with(&old_session)
                )
                .await,
                "Unauthenticated"
            )
            .await,
            "a refresh token the old password opened must not outlive the change"
        );
        assert_eq!(
            status, 200,
            "the bump ended every session, so the change is answered as done"
        );
    }

    /// The bump is the invalidation, so a bump that fails is the failure the
    /// caller hears about: the credential has changed, and an account whose
    /// version did not move still honours every session the old password
    /// opened.
    #[tokio::test]
    async fn a_failed_bump_is_not_reported_as_success() {
        use crate::blocks::auth::repo::users;

        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "abe@example.com", "original-horse-battery1").await;
        let failing = FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.increment_field_where", users::TABLE)],
        );

        let msg = auth_msg("update", "/b/auth/api/change-password", &user_id);
        let out = handle(
            &failing,
            &msg,
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        assert!(
            output_is_error(out, "Internal").await,
            "a password change whose sessions did not end must not report success"
        );
        assert_eq!(users::auth_version(&ctx, &user_id).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn successful_revocation_still_reports_success() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "bob@example.com", "original-horse-battery1").await;

        let msg = auth_msg("update", "/b/auth/api/change-password", &user_id);
        let out = handle(
            &ctx,
            &msg,
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        let buf = collect_or_panic(out).await;
        let json: serde_json::Value = serde_json::from_slice(&buf.body).unwrap();
        assert_eq!(json["message"], "Password changed successfully");
    }

    /// P2c: a successful password change must bump the user's auth_version
    /// so already-issued access JWTs stop authenticating (see
    /// `crate::crypto::extract_auth_meta`'s `auth_version` check).
    #[tokio::test]
    async fn successful_password_change_bumps_auth_version() {
        use crate::blocks::auth::repo::users;

        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "carol@example.com", "original-horse-battery1").await;
        assert_eq!(users::auth_version(&ctx, &user_id).await.unwrap(), 0);

        let msg = auth_msg("update", "/b/auth/api/change-password", &user_id);
        let out = handle(
            &ctx,
            &msg,
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;
        let _ = collect_or_panic(out).await;

        assert_eq!(
            users::auth_version(&ctx, &user_id).await.unwrap(),
            1,
            "password change must bump auth_version"
        );
    }

    /// The existence probe folded `Ok(None)` and `Err` into one
    /// `404 User not found`, four lines above a credential lookup that has
    /// always three-way branched. An outage told a signed-in caller their
    /// account was gone and their password was left unchanged.
    #[tokio::test]
    async fn an_unreadable_user_row_is_an_outage_not_a_missing_account() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "dana@example.com", "original-horse-battery1").await;
        // The existence probe is the handler's first database read; the
        // password policy above it reads configuration only.
        let failing = ctx.break_reads();

        let msg = auth_msg("update", "/b/auth/api/change-password", &user_id);
        let out = handle(
            &failing,
            &msg,
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        assert!(
            output_is_error(out, "Internal").await,
            "a failed existence probe must not answer 404"
        );
    }

    /// A stored hash the crypto service cannot check is this deployment's
    /// fault. The caller is already signed in as the account, so there is
    /// nothing to hide from them, and "Current password is incorrect" would
    /// tell them their correct password is wrong. It is a 500, logged.
    #[tokio::test]
    async fn a_malformed_stored_hash_is_a_500_not_an_incorrect_password() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "gina@example.com", "original-horse-battery1").await;
        local_credentials::update_password(&ctx, &user_id, "not-a-password-hash")
            .await
            .expect("overwrite the stored hash");

        let out = handle(
            &ctx,
            &auth_msg("update", "/b/auth/api/change-password", &user_id),
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        assert_eq!(output_http_status(out).await, 500);
    }

    /// A comparison the crypto service could not run is the classified 503,
    /// not a wrong current password.
    #[tokio::test]
    async fn a_crypto_outage_is_a_503_not_an_incorrect_password() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "hank@example.com", "original-horse-battery1").await;
        let down = FailingServiceOpContext::failing_with(
            ctx,
            "wafer-run/crypto",
            vec!["crypto.compare_hash"],
            wafer_run::WaferError::new(wafer_run::ErrorCode::Unavailable, "crypto is down"),
        );

        let out = handle(
            &down,
            &auth_msg("update", "/b/auth/api/change-password", &user_id),
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        assert_eq!(output_http_status(out).await, 503);
    }

    /// A password hasher that cannot be reached while the new password is
    /// hashed is a 503 the caller may retry, and nothing changes: the old
    /// password still works, `auth_version` is not bumped.
    #[tokio::test]
    async fn an_unreachable_password_hasher_is_a_503_and_changes_nothing() {
        use crate::{blocks::auth::repo::users, test_support::HasherFault};

        let (ctx, hasher) = TestContext::with_auth_and_faulty_hasher().await;
        let user_id = signup_user(&ctx, "iris@example.com", "original-horse-battery1").await;
        let stored = local_credentials::find_by_user_id(&ctx, &user_id)
            .await
            .unwrap()
            .expect("credential")
            .password_hash;
        hasher.fail_hash(Some(HasherFault::Unreachable));

        let out = handle(
            &ctx,
            &auth_msg("update", "/b/auth/api/change-password", &user_id),
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        assert_eq!(output_http_status(out).await, 503);
        assert_eq!(
            local_credentials::find_by_user_id(&ctx, &user_id)
                .await
                .unwrap()
                .expect("credential")
                .password_hash,
            stored,
            "the stored password is untouched"
        );
        assert_eq!(users::auth_version(&ctx, &user_id).await.unwrap(), 0);
    }

    /// Guard: a hashing fault retrying does not fix stays a 500 (passes
    /// before and after the `Unavailable` classification by design).
    #[tokio::test]
    async fn a_broken_password_hasher_is_a_500() {
        use crate::test_support::HasherFault;

        let (ctx, hasher) = TestContext::with_auth_and_faulty_hasher().await;
        let user_id = signup_user(&ctx, "jack@example.com", "original-horse-battery1").await;
        hasher.fail_hash(Some(HasherFault::Broken));

        let out = handle(
            &ctx,
            &auth_msg("update", "/b/auth/api/change-password", &user_id),
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        assert_eq!(output_http_status(out).await, 500);
    }

    /// What a change ends, through the production request path
    /// (`TestContext::request`: the router block and
    /// `pipeline::handle_request`, which verifies each credential the way a
    /// deployment does).
    mod what_a_change_ends {
        use crate::{
            blocks::auth::repo::{sessions, users::InitialRole},
            test_support::{anon_msg, api_key_header, Session, TestContext},
        };

        const EMAIL: &str = "changes@example.com";
        const PASSWORD: &str = "original-horse-battery1";
        const NEW_PASSWORD: &str = "new-horse-battery-2026";

        async fn status(out: wafer_run::OutputStream) -> u16 {
            wafer_block::http_codec::collect_http_response(out)
                .await
                .status
        }

        /// `GET /b/auth/api/me`, a route only a signed-in caller reaches.
        async fn me_with_session(ctx: &TestContext, session: &Session) -> u16 {
            status(
                ctx.request(session.bearer(anon_msg("retrieve", "/b/auth/api/me")))
                    .await,
            )
            .await
        }

        async fn me_with_key(ctx: &TestContext, key: &str) -> u16 {
            status(
                ctx.request(api_key_header(anon_msg("retrieve", "/b/auth/api/me"), key))
                    .await,
            )
            .await
        }

        /// `POST /b/auth/api/api-keys` as the pipeline's credential step leaves
        /// it for `authorization`: the step itself runs here, in the router's
        /// frame, and writes the `auth.*` meta the handler reads.
        async fn verified_create(ctx: &TestContext, authorization: &str) -> wafer_run::Message {
            let router = ctx
                .clone()
                .running_as(crate::blocks::router::ROUTER_BLOCK_ID);
            let mut msg = anon_msg("create", "/b/auth/api/api-keys");
            if let Some(token) = authorization.strip_prefix("ApiKey ") {
                crate::blocks::auth::authenticate_api_key(&router, token, &mut msg)
                    .await
                    .expect("the key check completes");
            } else {
                let secret = wafer_run::context::Context::config_get(
                    &router,
                    crate::blocks::auth::JWT_SECRET_KEY,
                )
                .unwrap_or("")
                .to_string();
                let iss = crate::crypto::expected_issuer(&router)
                    .await
                    .expect("read the issuer");
                crate::crypto::extract_auth_meta(&router, authorization, &secret, &iss, &mut msg)
                    .await
                    .expect("the token check completes");
            }
            assert!(!msg.user_id().is_empty(), "the credential verified");
            msg
        }

        /// A key-creation request authenticated before a password change,
        /// whose insert lands after the change revoked the account's keys,
        /// must not leave a working key behind. `handle_create` checks the
        /// presenting credential again after its insert: the change ended it,
        /// so the new key is revoked and the request refused. Driven for both
        /// credentials that can create a key: an access token (its login
        /// family) and an API key (its own row).
        #[tokio::test]
        async fn a_key_minted_across_the_change_does_not_survive_it() {
            use crate::blocks::auth::repo::api_keys;

            let ctx = TestContext::with_auth().await.with_sign_in_added();
            let user_id = ctx.seed_account(EMAIL, PASSWORD, InitialRole::User).await;
            let here = ctx.sign_in(EMAIL, PASSWORD).await;
            let attacker = ctx.sign_in(EMAIL, PASSWORD).await;
            let attacker_key = attacker.create_api_key(&ctx, "attacker").await;

            // R1: two in-flight creates pass the pipeline's credential step,
            // one on the attacker's session and one on the attacker's key.
            let on_session =
                verified_create(&ctx, &format!("Bearer {}", attacker.access_token)).await;
            let on_key = verified_create(&ctx, &format!("ApiKey {attacker_key}")).await;

            // C: the owner changes the password: rows, tokens, keys, bump.
            let changed = ctx
                .request_json(
                    here.bearer(anon_msg("create", "/b/auth/api/change-password")),
                    &serde_json::json!({
                        "current_password": PASSWORD,
                        "new_password": NEW_PASSWORD,
                    }),
                )
                .await;
            assert_eq!(status(changed).await, 200);

            // R2: each in-flight create reaches its insert.
            let auth_ui = ctx
                .clone()
                .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
            for (credential, msg) in [("session", &on_session), ("API key", &on_key)] {
                let out = crate::blocks::auth_ui::api::api_keys::handle_create(
                    &auth_ui,
                    msg,
                    wafer_run::InputStream::from_bytes(br#"{"name":"late"}"#.to_vec()),
                )
                .await;
                assert_eq!(
                    status(out).await,
                    401,
                    "a create on the attacker's {credential} is refused once the change ended it"
                );
            }
            let keys = api_keys::list_for_user(&ctx.fixture(), &user_id)
                .await
                .unwrap();
            assert_eq!(keys.len(), 3, "the attacker's key and the two late ones");
            assert!(
                keys.iter().all(|key| key.is_revoked()),
                "no key of the account survives the change: {keys:?}"
            );
        }

        /// The re-check does not change who may create a key: a live session
        /// and a live API key both still can.
        #[tokio::test]
        async fn a_live_session_or_api_key_still_creates_keys() {
            let ctx = TestContext::with_auth().await.with_sign_in_added();
            ctx.seed_account(EMAIL, PASSWORD, InitialRole::User).await;
            let session = ctx.sign_in(EMAIL, PASSWORD).await;
            let key = session.create_api_key(&ctx, "first").await;
            let created = ctx
                .request_json(
                    api_key_header(anon_msg("create", "/b/auth/api/api-keys"), &key),
                    &serde_json::json!({ "name": "second" }),
                )
                .await;
            assert_eq!(status(created).await, 200, "an API key creates a key");
        }

        /// Decision 2: a password change revokes the account's API keys, and
        /// it signs out every device, the one that made the change included
        /// (it re-issues none). A key checks no `auth_version`, so without
        /// the revocation it would outlive the change.
        #[tokio::test]
        async fn a_password_change_ends_every_device_and_api_key() {
            let ctx = TestContext::with_auth().await.with_sign_in_added();
            let user_id = ctx.seed_account(EMAIL, PASSWORD, InitialRole::User).await;
            let here = ctx.sign_in(EMAIL, PASSWORD).await;
            let elsewhere = ctx.sign_in(EMAIL, PASSWORD).await;
            let key = here.create_api_key(&ctx, "ci").await;
            assert_eq!(me_with_key(&ctx, &key).await, 200, "the key works");
            assert_eq!(me_with_session(&ctx, &elsewhere).await, 200);

            let changed = ctx
                .request_json(
                    here.bearer(anon_msg("create", "/b/auth/api/change-password")),
                    &serde_json::json!({
                        "current_password": PASSWORD,
                        "new_password": NEW_PASSWORD,
                    }),
                )
                .await;
            assert_eq!(status(changed).await, 200, "the change is answered as done");

            assert_eq!(
                me_with_key(&ctx, &key).await,
                401,
                "an API key minted before the change no longer authenticates"
            );
            assert_eq!(me_with_session(&ctx, &elsewhere).await, 401);
            assert_eq!(me_with_session(&ctx, &here).await, 401);
            assert!(
                sessions::list_for_user(&ctx.fixture(), &user_id)
                    .await
                    .unwrap()
                    .is_empty(),
                "no device is listed as signed in after the change"
            );
        }
    }

    /// A key revocation that fails leaves a credential nothing else ends — a
    /// key checks no `auth_version` — so the change is not answered as done.
    #[tokio::test]
    async fn a_failed_api_key_revocation_is_not_reported_as_success() {
        use crate::blocks::auth::repo::{api_keys, users};

        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "keys@example.com", "original-horse-battery1").await;
        let failing = FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.update_where", api_keys::TABLE)],
        );

        let msg = auth_msg("update", "/b/auth/api/change-password", &user_id);
        let out = handle(
            &failing,
            &msg,
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        assert!(
            output_is_error(out, "Internal").await,
            "a change whose API keys were not revoked must not report success"
        );
        assert_eq!(
            users::auth_version(&ctx, &user_id).await.unwrap(),
            1,
            "the steps after the failed one still run"
        );
    }

    /// A session-row deletion that fails leaves every device signed in (and
    /// listed) until the bump is read, so the change is not answered as done.
    #[tokio::test]
    async fn a_failed_session_row_deletion_is_not_reported_as_success() {
        use crate::blocks::auth::repo::{sessions, users};

        let ctx = TestContext::with_auth_and_crypto().await;
        let user_id = signup_user(&ctx, "rows@example.com", "original-horse-battery1").await;
        let failing = FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.delete_where_count", sessions::TABLE)],
        );

        let msg = auth_msg("update", "/b/auth/api/change-password", &user_id);
        let out = handle(
            &failing,
            &msg,
            body("original-horse-battery1", "new-horse-battery-2026"),
        )
        .await;

        assert!(
            output_is_error(out, "Internal").await,
            "a change whose devices were not signed out must not report success"
        );
        assert_eq!(
            users::auth_version(&ctx, &user_id).await.unwrap(),
            1,
            "the steps after the failed one still run"
        );
    }
}
