//! GET /b/auth/change-password — relocated from auth/pages/mod.rs::change_password_page in Task 5.

use maud::{html, PreEscaped};
use wafer_run::{context::Context, Message, OutputStream};

use super::{pw_field, pw_toggle_js, site_config};
use crate::{
    blocks::auth::brand_panel,
    ui::{self, templates::auth_split},
};

pub async fn handle(ctx: &dyn Context, _msg: &Message) -> OutputStream {
    let config = site_config(ctx);
    let app_name = &config.app_name;
    let logo_url = &config.logo_url;

    let markup = ui::layout::page(
        "Change Password",
        &config,
        auth_split(
            brand_panel(&config, "Update your password."),
            html! {
                div .login-container {
                    div .login-logo {
                        @if !logo_url.is_empty() {
                            img .logo-image src=(logo_url) alt=(app_name);
                        } @else {
                            span .login-app-name { (app_name) }
                        }
                        p .login-subtitle { "Change your password" }
                    }

                    div #error .login-error style="display:none" {}

                    div #success style="background:#ecfdf5;border:1px solid #a7f3d0;border-radius:8px;padding:1rem;text-align:center;display:none" {
                        p style="font-size:.875rem;color:#16a34a;margin:0 0 1rem;font-weight:500" {
                            "Password changed successfully!"
                        }
                        button .login-button onclick="history.back()" style="width:auto;display:inline-block;padding:.625rem 1.25rem" {
                            "Go Back"
                        }
                    }

                    form #form .login-form onsubmit="return handleChange(event)" {
                        div .form-group {
                            label .form-label for="current" { "Current Password" }
                            (pw_field("current", "Enter your current password", None))
                        }

                        div .form-group {
                            label .form-label for="newpw" { "New Password" }
                            (pw_field("newpw", "Min 8 characters", Some("8")))
                        }

                        div .form-group {
                            label .form-label for="confirm" { "Confirm New Password" }
                            (pw_field("confirm", "Repeat new password", Some("8")))
                        }

                        button .login-button type="submit" #btn { "Change Password" }
                    }

                    div style="text-align:center;margin-top:1rem" {
                        a .btn .btn-ghost href="javascript:history.back()" { "Cancel" }
                    }
                }

                script { (PreEscaped(pw_toggle_js())) }
                script { (PreEscaped(r#"
var $=function(id){return document.getElementById(id)};
function showErr(m){var e=$('error');e.textContent=m;e.style.display='flex'}
async function handleChange(ev){
  ev.preventDefault();
  var btn=$('btn');$('error').style.display='none';
  var pw=$('newpw').value,cf=$('confirm').value;
  if(pw!==cf){showErr('New passwords do not match.');return false}
  if(pw.length<8){showErr('Password must be at least 8 characters.');return false}
  btn.disabled=true;btn.textContent='Changing...';
  try{
    var r=await fetch('/b/auth/api/change-password',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({current_password:$('current').value,new_password:pw})});
    var d=await r.json();
    if(!r.ok||d.error){showErr((d.error&&d.error.message)||d.error||'Failed to change password');btn.disabled=false;btn.textContent='Change Password';return false}
    $('form').style.display='none';$('success').style.display='block';
  }catch(ex){showErr('Something went wrong');btn.disabled=false;btn.textContent='Change Password'}
  return false;
}
"#)) }
            },
        ),
    );

    ui::html_response(markup)
}
