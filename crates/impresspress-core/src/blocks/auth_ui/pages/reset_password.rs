//! GET /b/auth/reset-password — relocated from auth/login.rs::handle_reset_password_form
//! in Task 5.

use maud::{html, PreEscaped};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{blocks::auth::brand_panel, ui, ui::templates::auth_split};

pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let logo_url = ctx
        .config_get("WAFER_RUN_SHARED__AUTH_LOGO_URL")
        .unwrap_or("")
        .to_string();
    let app_name = ctx
        .config_get("WAFER_RUN_SHARED__APP_NAME")
        .unwrap_or("Impresspress")
        .to_string();

    let token = msg.get_meta("req.query.token").to_string();
    if token.is_empty() {
        return html_respond(
            "Invalid Link",
            "This password reset link is invalid.",
            false,
            &logo_url,
            &app_name,
        );
    }

    let config = ui::SiteConfig {
        app_name: ctx
            .config_get("WAFER_RUN_SHARED__APP_NAME")
            .unwrap_or("Impresspress")
            .to_string(),
        logo_url: logo_url.clone(),
        logo_icon_url: String::new(),
        favicon_url: crate::ui::assets::favicon_url().to_string(),
        embedded_scripts: Vec::new(),
    };

    let markup = ui::layout::page(
        "Reset Password",
        &config,
        auth_split(
            brand_panel(&config, "Reset your password."),
            html! {
                div .login-container {
                    div .login-logo {
                        @if !logo_url.is_empty() {
                            img .logo-image src=(logo_url) alt=(config.app_name);
                        }
                        p .login-subtitle { "Reset your password" }
                    }

                    div #error .login-error style="display:none" {}
                    div #success style="background:#ecfdf5;border:1px solid #a7f3d0;border-radius:8px;padding:.75rem;margin-bottom:1rem;font-size:.813rem;color:#059669;display:none" {}

                    form #form .login-form onsubmit="return handleReset(event)" {
                        input type="hidden" #reset-token name="token" value=(token);

                        div .form-group {
                            label .form-label for="password" { "New Password" }
                            input .form-input type="password" #password required minlength="8" placeholder="Min 8 characters";
                        }
                        div .form-group {
                            label .form-label for="confirm" { "Confirm Password" }
                            input .form-input type="password" #confirm required minlength="8" placeholder="Repeat password";
                        }

                        button .login-button type="submit" #btn { "Reset Password" }
                    }
                }

                script { (PreEscaped(r#"
var $=function(id){return document.getElementById(id)};
async function handleReset(e){
  e.preventDefault();
  var pw=$('password').value,cf=$('confirm').value;
  var err=$('error'),suc=$('success'),btn=$('btn');
  var token=$('reset-token').value;
  err.style.display='none';suc.style.display='none';
  if(pw!==cf){err.textContent='Passwords do not match.';err.style.display='flex';return false;}
  if(pw.length<8){err.textContent='Password must be at least 8 characters.';err.style.display='flex';return false;}
  btn.disabled=true;btn.textContent='Resetting...';
  try{
    var r=await fetch('/b/auth/api/reset-password',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({token:token,new_password:pw})});
    var d=await r.json();
    if(d.error){err.textContent=d.error.message||d.error;err.style.display='flex';}
    else{suc.textContent='Password reset successfully. You can now sign in.';suc.style.display='block';$('form').style.display='none';
      setTimeout(function(){window.location.href='/b/auth/login';},2000);}
  }catch(ex){err.textContent='Something went wrong.';err.style.display='flex';}
  btn.disabled=false;btn.textContent='Reset Password';
  return false;
}
"#)) }
            },
        ),
    );

    ui::html_response(markup)
}

/// Return an HTML page response (for the invalid-token failure case).
fn html_respond(
    title: &str,
    message: &str,
    success: bool,
    logo_url: &str,
    app_name: &str,
) -> OutputStream {
    let color = if success { "#10b981" } else { "#ef4444" };
    let config = ui::SiteConfig {
        app_name: app_name.to_string(),
        logo_url: logo_url.to_string(),
        logo_icon_url: String::new(),
        favicon_url: crate::ui::assets::favicon_url().to_string(),
        embedded_scripts: Vec::new(),
    };
    let markup = ui::layout::page(
        title,
        &config,
        auth_split(
            brand_panel(&config, "Reset your password."),
            html! {
                div .login-container {
                    div .login-logo {
                        @if !logo_url.is_empty() {
                            img .logo-image src=(logo_url) alt=(config.app_name);
                        }
                    }
                    div style="text-align:center" {
                        div style={"width:48px;height:48px;background:" (color) "15;border-radius:50%;display:flex;align-items:center;justify-content:center;margin:0 auto 1rem;font-size:1.5rem;color:" (color)} {
                            @if success { "✓" } @else { "✗" }
                        }
                        h2 style="font-size:1.25rem;font-weight:700;margin:0 0 .5rem" { (title) }
                        p .login-subtitle style="line-height:1.6;margin:0 0 1.5rem" { (message) }
                        a .login-button href="/b/auth/login" style="display:inline-block;width:auto;padding:.625rem 1.25rem;text-decoration:none" {
                            "Go to Sign In"
                        }
                    }
                }
            },
        ),
    );
    ui::html_response(markup)
}
