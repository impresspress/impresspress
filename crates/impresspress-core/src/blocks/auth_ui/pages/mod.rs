//! SSR page handlers for the auth-ui block.
//!
//! Each leaf module hosts one page handler relocated from the legacy
//! `auth/pages/` tree in Task 5 of Plan A2 PR 5.

pub mod bootstrap;
pub mod change_password;
pub mod login;
pub mod orgs;
pub mod reset_password;
pub mod settings;
pub mod signup;

use maud::{html, Markup};
use wafer_run::context::Context;

use crate::{
    blocks::auth_ui::OAUTH_REDIRECT_URI_KEY,
    ui::{self, SiteConfig},
};

/// The auth pages' site config.
///
/// Was a synchronous near-copy of [`SiteConfig::load`] reading
/// `ctx.config_get`, which serves the boot-time snapshot: an admin's saved
/// branding did not reach the login page until the process restarted, and on
/// Cloudflare never reached it at all. Delegates to the one async loader now,
/// so these pages cannot drift from the rest of the site.
pub(super) async fn site_config(ctx: &dyn Context) -> Result<SiteConfig, wafer_run::WaferError> {
    SiteConfig::load_for_auth(ctx).await
}

/// True if the provider has all three credentials needed for the modern
/// single-callback OAuth flow (`/auth/oauth/login`, `/auth/oauth/callback`):
///
/// - `IMPRESSPRESS__AUTH_UI__OAUTH_<PROVIDER>_CLIENT_ID`
/// - `IMPRESSPRESS__AUTH_UI__OAUTH_<PROVIDER>_CLIENT_SECRET`
/// - `IMPRESSPRESS__AUTH_UI__OAUTH_REDIRECT_URI` (single URI, provider-agnostic;
///   the provider is encoded in the signed `state` JWT)
///
/// These match what `oauth.rs` actually reads when building the auth_url.
///
/// Read through the config client, not `ctx.config_get`. These three are
/// admin-editable rows in the variables table, and that snapshot is frozen at
/// boot: an operator who pasted OAuth credentials into the admin UI got no
/// OAuth buttons until the process restarted, and on Cloudflare never, since
/// no D1 row reaches that surface. Same defect as the branding reads, on a
/// page where the symptom is a missing sign-in button rather than a wrong
/// colour.
///
/// A failed read is returned: a button hidden because the config block
/// refused the read would look exactly like a provider nobody configured.
pub(in crate::blocks::auth_ui) async fn oauth_provider_configured(
    ctx: &dyn Context,
    provider: &str,
) -> Result<bool, wafer_run::WaferError> {
    use wafer_core::clients::config;

    let up = provider.to_ascii_uppercase();
    let client_id = config::get_default(
        ctx,
        &format!("IMPRESSPRESS__AUTH_UI__OAUTH_{up}_CLIENT_ID"),
        "",
    )
    .await?;
    if client_id.is_empty() {
        return Ok(false);
    }
    let client_secret = config::get_default(
        ctx,
        &format!("IMPRESSPRESS__AUTH_UI__OAUTH_{up}_CLIENT_SECRET"),
        "",
    )
    .await?;
    if client_secret.is_empty() {
        return Ok(false);
    }
    Ok(!config::get_default(ctx, OAUTH_REDIRECT_URI_KEY, "")
        .await?
        .is_empty())
}

/// Display label for an OAuth provider button.
pub(super) fn oauth_provider_label(provider: &str) -> &'static str {
    match provider {
        "github" => "GitHub",
        "google" => "Google",
        "microsoft" => "Microsoft",
        _ => "OAuth",
    }
}

/// Inline SVG glyph for an OAuth provider button. Sized to sit beside text.
pub(super) fn oauth_provider_icon(provider: &str) -> Markup {
    match provider {
        "github" => html! {
            svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor" aria-hidden="true" {
                path d="M12 0C5.37 0 0 5.37 0 12c0 5.31 3.435 9.795 8.205 11.385.6.105.825-.255.825-.57 0-.285-.015-1.23-.015-2.235-3.015.555-3.795-.735-4.035-1.41-.135-.345-.72-1.41-1.23-1.695-.42-.225-1.02-.78-.015-.795.945-.015 1.62.87 1.845 1.23 1.08 1.815 2.805 1.305 3.495.99.105-.78.42-1.305.765-1.605-2.67-.3-5.46-1.335-5.46-5.925 0-1.305.465-2.385 1.23-3.225-.12-.3-.54-1.53.12-3.18 0 0 1.005-.315 3.3 1.23.96-.27 1.98-.405 3-.405s2.04.135 3 .405c2.295-1.56 3.3-1.23 3.3-1.23.66 1.65.24 2.88.12 3.18.765.84 1.23 1.905 1.23 3.225 0 4.605-2.805 5.625-5.475 5.925.435.375.81 1.095.81 2.22 0 1.605-.015 2.895-.015 3.3 0 .315.225.69.825.57A12.02 12.02 0 0024 12c0-6.63-5.37-12-12-12z" {}
            }
        },
        // No bespoke marks for google/microsoft yet — fall back to a
        // neutral lock icon so the button still renders visually.
        _ => html! {
            svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" {
                rect width="18" height="11" x="3" y="11" rx="2" ry="2" {}
                path d="M7 11V7a5 5 0 0 1 10 0v4" {}
            }
        },
    }
}

/// Browser-side handler for OAuth buttons: a top-level navigation to the
/// start endpoint, which answers `302` to the provider.
///
/// Deliberately a navigation and not a `fetch`. The start endpoint sets the
/// cookie that binds the flow to this browser, and a cookie set on a `fetch`
/// response is only stored first-party when the page and the API share an
/// origin — see `oauth::start`. Navigating also means there is no JSON to
/// read and no error to surface here: a refused start renders the API's own
/// error response, and the buttons themselves are only rendered for
/// providers this deployment has configured (`oauth_provider_configured`).
pub(super) fn oauth_button_script() -> &'static str {
    r#"
document.addEventListener('click',function(e){
  if(!(e.target instanceof Element))return;
  var el=e.target.closest('[data-action="oauth-start"]');
  if(!el)return;
  e.preventDefault();
  var provider=el.getAttribute('data-provider')||'';
  window.location.href='/b/auth/oauth/login?provider='+encodeURIComponent(provider);
});
"#
}

/// Password field with visibility toggle.
pub(super) fn pw_field(id: &str, placeholder: &str, minlength: Option<&str>) -> Markup {
    html! {
        div .pw-wrap {
            input
                type="password"
                class="form-input"
                id=(id)
                placeholder=(placeholder)
                required
                minlength=[minlength];
            // The reveal is chrome's shared `reveal-toggle` verb (the modal
            // section of `ui/assets/chrome.js`), which every auth page loads
            // through `ui::layout::page`. With no `data-reveal-show`/`-hide`
            // operands the button keeps its one static label for both states,
            // which is what the `togglePw(this)` helper this replaced did.
            button type="button" class="pw-toggle" aria-label="Toggle password visibility"
                data-action="reveal-toggle" data-reveal-target=(id) {
                (ui::icons::eye_off())
            }
        }
    }
}

/// JS that drives the login + forgot-password forms.
///
/// On browser (wasm32) targets, the server runs inside a Service Worker and
/// browsers do not persist `Set-Cookie` from SW-synthetic responses. We set
/// the auth cookie from the response body client-side in that case. On native
/// targets the server's `Set-Cookie` already works, so we emit a version of
/// this JS without the client-side assignment — no HttpOnly regression.
///
/// `#error`/`#info` are `components::alert`, which starts `hidden`. Toggling
/// visibility must clear/set the `hidden` IDL property, not `style.display`
/// — see the doc comment on `oauth_button_script`.
pub(super) fn login_script() -> &'static str {
    #[cfg(target_arch = "wasm32")]
    {
        r#"
var $=function(id){return document.getElementById(id)};
function showErr(m){var e=$('error');e.textContent=m;e.hidden=false;$('info').hidden=true}
function showInfo(m){var i=$('info');i.textContent=m;i.hidden=false;$('error').hidden=true}
async function handleLogin(ev){
  ev.preventDefault();
  var btn=$('btn');btn.disabled=true;btn.textContent='Signing in...';
  $('error').hidden=true;$('info').hidden=true;
  try{
    var r=await fetch('/b/auth/api/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({email:$('email').value,password:$('password').value})});
    var d=await r.json();
    if(!r.ok||d.error){showErr((d.error&&d.error.message)||d.error||d.message||'Invalid credentials');btn.disabled=false;btn.textContent='Sign In';return false}
    // Service-worker synthetic responses don't persist Set-Cookie, so set the
    // auth cookie client-side from the response body.
    if(d.access_token){
      var secure=location.protocol==='https:'?'; Secure':'';
      var maxAge=d.expires_in||1800;
      document.cookie='auth_token='+d.access_token+'; Path=/; SameSite=Lax; Max-Age='+maxAge+secure;
    }
    var redir=$('redirect').value||d.default_redirect||'/';
    window.location.href=redir;
  }catch(ex){showErr('Something went wrong');btn.disabled=false;btn.textContent='Sign In'}
  return false;
}
async function handleForgot(){
  var email=$('email').value.trim();
  if(!email){showErr('Enter your email address first.');return}
  $('error').hidden=true;$('info').hidden=true;
  try{await fetch('/b/auth/api/forgot-password',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({email:email})})}catch(e){}
  showInfo('If that email is registered, a password reset link has been sent.');
}
document.addEventListener('submit',function(e){if(e.target&&e.target.id==='form')handleLogin(e)});
document.addEventListener('click',function(e){
  if(!(e.target instanceof Element))return;
  var el=e.target.closest('[data-action="auth-forgot"]');
  if(el){e.preventDefault();handleForgot()}
});
"#
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        r#"
var $=function(id){return document.getElementById(id)};
function showErr(m){var e=$('error');e.textContent=m;e.hidden=false;$('info').hidden=true}
function showInfo(m){var i=$('info');i.textContent=m;i.hidden=false;$('error').hidden=true}
async function handleLogin(ev){
  ev.preventDefault();
  var btn=$('btn');btn.disabled=true;btn.textContent='Signing in...';
  $('error').hidden=true;$('info').hidden=true;
  try{
    var r=await fetch('/b/auth/api/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({email:$('email').value,password:$('password').value})});
    var d=await r.json();
    if(!r.ok||d.error){showErr((d.error&&d.error.message)||d.error||d.message||'Invalid credentials');btn.disabled=false;btn.textContent='Sign In';return false}
    var redir=$('redirect').value||d.default_redirect||'/';
    window.location.href=redir;
  }catch(ex){showErr('Something went wrong');btn.disabled=false;btn.textContent='Sign In'}
  return false;
}
async function handleForgot(){
  var email=$('email').value.trim();
  if(!email){showErr('Enter your email address first.');return}
  $('error').hidden=true;$('info').hidden=true;
  try{await fetch('/b/auth/api/forgot-password',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({email:email})})}catch(e){}
  showInfo('If that email is registered, a password reset link has been sent.');
}
document.addEventListener('submit',function(e){if(e.target&&e.target.id==='form')handleLogin(e)});
document.addEventListener('click',function(e){
  if(!(e.target instanceof Element))return;
  var el=e.target.closest('[data-action="auth-forgot"]');
  if(el){e.preventDefault();handleForgot()}
});
"#
    }
}

/// JS that drives the signup form.
///
/// Mirrors [`login_script`]'s wasm32/native split: on browser (wasm32)
/// targets the server runs inside a Service Worker and `Set-Cookie` from a
/// SW-synthetic response doesn't persist, so the auto-login cookie is also
/// set from the response body client-side there; native gets the version
/// without the client-side assignment since the server's `Set-Cookie`
/// already works.
///
/// Two outcomes from `POST /b/auth/api/signup`:
/// - `email_verified === false` (verification required): stays on the
///   signup page and shows the "check your email" panel — no auto-login
///   happened server-side, so there's nothing to navigate to yet. The
///   "Back to Sign In" link is rewritten to carry `?email=` so the user
///   doesn't have to retype it once they've verified.
/// - otherwise: the API already auto-logged the user in (tokens issued,
///   cookie set) — navigate straight to the role-aware `default_redirect`
///   the response computed, honoring an explicit `redirect` param first.
///   This replaces the old unconditional bounce to `/b/auth/login`, which
///   ignored the fact the user was already authenticated.
pub(super) fn signup_script() -> &'static str {
    #[cfg(target_arch = "wasm32")]
    {
        r#"
var $=function(id){return document.getElementById(id)};
function showErr(m){var e=$('error');e.textContent=m;e.hidden=false}
async function handleSignup(ev){
  ev.preventDefault();
  var btn=$('btn');btn.disabled=true;btn.textContent='Creating account...';
  $('error').hidden=true;
  var email=$('email').value,pw=$('password').value;
  try{
    var r=await fetch('/b/auth/api/signup',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({email:email,password:pw})});
    var d=await r.json();
    if(!r.ok||d.error){showErr((d.error&&d.error.message)||d.error||d.message||'Signup failed');btn.disabled=false;btn.textContent='Create Account';return false}
    if(d.email_verified===false){
      $('form').hidden=true;$('signin-link').hidden=true;
      $('verify-msg').textContent='We sent a verification link to '+email+'. Click the link to activate your account.';
      var back=$('back-to-signin');
      if(back){var qs='email='+encodeURIComponent(email);var r2=$('redirect').value;if(r2){qs+='&redirect='+encodeURIComponent(r2)}back.setAttribute('href','/b/auth/login?'+qs);}
      $('success').hidden=false;
    }else{
      if(d.access_token){
        var secure=location.protocol==='https:'?'; Secure':'';
        var maxAge=d.expires_in||1800;
        document.cookie='auth_token='+d.access_token+'; Path=/; SameSite=Lax; Max-Age='+maxAge+secure;
      }
      var redir=$('redirect').value||d.default_redirect||'/';
      window.location.href=redir;
    }
  }catch(ex){showErr('Something went wrong');btn.disabled=false;btn.textContent='Create Account'}
  return false;
}
document.addEventListener('submit',function(e){if(e.target&&e.target.id==='form')handleSignup(e)});
"#
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        r#"
var $=function(id){return document.getElementById(id)};
function showErr(m){var e=$('error');e.textContent=m;e.hidden=false}
async function handleSignup(ev){
  ev.preventDefault();
  var btn=$('btn');btn.disabled=true;btn.textContent='Creating account...';
  $('error').hidden=true;
  var email=$('email').value,pw=$('password').value;
  try{
    var r=await fetch('/b/auth/api/signup',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({email:email,password:pw})});
    var d=await r.json();
    if(!r.ok||d.error){showErr((d.error&&d.error.message)||d.error||d.message||'Signup failed');btn.disabled=false;btn.textContent='Create Account';return false}
    if(d.email_verified===false){
      $('form').hidden=true;$('signin-link').hidden=true;
      $('verify-msg').textContent='We sent a verification link to '+email+'. Click the link to activate your account.';
      var back=$('back-to-signin');
      if(back){var qs='email='+encodeURIComponent(email);var r2=$('redirect').value;if(r2){qs+='&redirect='+encodeURIComponent(r2)}back.setAttribute('href','/b/auth/login?'+qs);}
      $('success').hidden=false;
    }else{
      var redir=$('redirect').value||d.default_redirect||'/';
      window.location.href=redir;
    }
  }catch(ex){showErr('Something went wrong');btn.disabled=false;btn.textContent='Create Account'}
  return false;
}
document.addEventListener('submit',function(e){if(e.target&&e.target.id==='form')handleSignup(e)});
"#
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::auth_ui::{
            OAUTH_GITHUB_CLIENT_ID_KEY, OAUTH_GITHUB_CLIENT_SECRET_KEY, OAUTH_REDIRECT_URI_KEY,
        },
        config_vars::{
            APP_NAME_KEY, AUTH_LOGO_URL_KEY, DEFAULT_APP_NAME, EMBEDDED_SCRIPTS_KEY, LOGO_URL_KEY,
        },
        test_support::TestContext,
    };

    #[tokio::test]
    async fn site_config_reads_from_ctx_config_get_with_defaults() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let cfg = site_config(&ctx).await.expect("site config");

        assert_eq!(cfg.app_name, DEFAULT_APP_NAME);
        assert_eq!(cfg.logo_url, "", "no wordmark image by default");
        assert_eq!(cfg.logo_icon_url, crate::ui::assets::logo_icon_url());
        assert_eq!(cfg.favicon_url, crate::ui::assets::favicon_url());
        assert!(cfg.embedded_scripts.is_empty());
    }

    #[tokio::test]
    async fn site_config_picks_auth_logo_when_set() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.set_config(AUTH_LOGO_URL_KEY, "https://example.com/auth.png");
        ctx.set_config(LOGO_URL_KEY, "https://example.com/main.png");

        let cfg = site_config(&ctx).await.expect("site config");
        assert_eq!(cfg.logo_url, "https://example.com/auth.png");
    }

    #[tokio::test]
    async fn site_config_falls_back_to_logo_url_when_auth_logo_empty() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.set_config(LOGO_URL_KEY, "https://example.com/main.png");

        let cfg = site_config(&ctx).await.expect("site config");
        assert_eq!(cfg.logo_url, "https://example.com/main.png");
    }

    #[tokio::test]
    async fn site_config_app_name_override() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.set_config(APP_NAME_KEY, "MyApp");

        let cfg = site_config(&ctx).await.expect("site config");
        assert_eq!(cfg.app_name, "MyApp");
    }

    #[tokio::test]
    async fn site_config_embedded_scripts_splits_csv() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.set_config(
            EMBEDDED_SCRIPTS_KEY,
            "https://a.example.com/a.js, https://b.example.com/b.js,",
        );

        let cfg = site_config(&ctx).await.expect("site config");
        assert_eq!(
            cfg.embedded_scripts,
            vec![
                "https://a.example.com/a.js".to_string(),
                "https://b.example.com/b.js".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn oauth_provider_configured_requires_all_three_keys() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.set_config(OAUTH_GITHUB_CLIENT_ID_KEY, "id");
        ctx.set_config(OAUTH_GITHUB_CLIENT_SECRET_KEY, "secret");
        assert!(
            !oauth_provider_configured(&ctx, "github")
                .await
                .expect("config read"),
            "should be false without REDIRECT_URI"
        );

        ctx.set_config(OAUTH_REDIRECT_URI_KEY, "https://example.com/cb");
        assert!(
            oauth_provider_configured(&ctx, "github")
                .await
                .expect("config read"),
            "should be true once all three are set"
        );
    }

    #[tokio::test]
    async fn oauth_provider_configured_false_when_missing_any_key() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        assert!(!oauth_provider_configured(&ctx, "github")
            .await
            .expect("config read"));
        assert!(!oauth_provider_configured(&ctx, "google")
            .await
            .expect("config read"));
        assert!(!oauth_provider_configured(&ctx, "microsoft")
            .await
            .expect("config read"));
    }
}
