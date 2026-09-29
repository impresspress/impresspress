//! GET /b/auth/change-password — relocated from auth/pages/mod.rs::change_password_page in Task 5.

use maud::{html, PreEscaped};
use wafer_run::{context::Context, Message, OutputStream};

use super::{pw_field, site_config};
use crate::ui::{self, components::auth_panel, templates::auth_split};

pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let config = match site_config(ctx).await {
        Ok(site) => site,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "page: site config read failed")
        }
    };

    let markup = ui::layout::page(
        "Change Password",
        &config,
        auth_split(
            auth_panel(&config, Some("Update your password.")),
            html! {
                div .login-container {
                    div #error .login-error hidden {}

                    div #success .login-success .login-success--centered hidden {
                        p .change-password-success-text {
                            "Password changed successfully!"
                        }
                        button .login-button .auth-status__action data-action="history-back" {
                            "Go Back"
                        }
                    }

                    form #form .login-form {
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

                    div .text-center .mt-4 {
                        // The one `javascript:` URL in the tree, and the same
                        // hazard class as the `on*=` attributes this change
                        // removes: it goes back, which is an action, so it is
                        // a button that declares `history-back`.
                        button .btn .btn--ghost type="button" data-action="history-back" { "Cancel" }
                    }
                }

                script { (PreEscaped(r#"
var $=function(id){return document.getElementById(id)};
function showErr(m){var e=$('error');e.textContent=m;e.hidden=false}
async function handleChange(ev){
  ev.preventDefault();
  var btn=$('btn');$('error').hidden=true;
  var pw=$('newpw').value,cf=$('confirm').value;
  if(pw!==cf){showErr('New passwords do not match.');return false}
  if(pw.length<8){showErr('Password must be at least 8 characters.');return false}
  btn.disabled=true;btn.textContent='Changing...';
  try{
    var r=await fetch('/b/auth/api/change-password',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({current_password:$('current').value,new_password:pw})});
    var d=await r.json();
    if(!r.ok||d.error){showErr((d.error&&d.error.message)||d.error||'Failed to change password');btn.disabled=false;btn.textContent='Change Password';return false}
    $('form').hidden=true;$('success').hidden=false;
  }catch(ex){showErr('Something went wrong');btn.disabled=false;btn.textContent='Change Password'}
  return false;
}
document.addEventListener('submit',function(e){if(e.target&&e.target.id==='form')handleChange(e)});
document.addEventListener('click',function(e){
  if(!(e.target instanceof Element))return;
  if(!e.target.closest('[data-action="history-back"]'))return;
  e.preventDefault();
  history.back();
});
"#)) }
            },
        ),
    );

    ui::html_response(markup)
}
