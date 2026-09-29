// Guarded, because this whole script re-runs on every htmx partial swap: a tab
// navigation asks for the page with `HX-Request`, `ui::shell_page` answers with
// the body verbatim (`ui/mod.rs:226`), htmx executes the scripts in what it
// swapped in, and `document` outlives the swap. Without the flag a user who
// visits a second tab and comes back has this listener bound twice and every
// mutating action fires twice. The declarations below are safe to re-run --
// re-declaring a function replaces it -- so only the registration is guarded.
(function(){
  if(window.__sellerAdminDelegated)return;
  window.__sellerAdminDelegated=true;
  document.addEventListener('click',function(e){
    if(!(e.target instanceof Element))return;
    var el=e.target.closest('[data-action="psa-set-state"]');
    if(el)adminSellerSetState(el);
  });
})();
async function adminSellerSetState(button){if(window.__sellerAdminConfig.action==='suspend'&&!window.confirm('Suspend this seller? Active offers and Payment Links will be archived in Stripe before local access is revoked.'))return;button.disabled=true;var original=button.textContent;button.textContent='Working…';var target=document.getElementById('seller-admin-error');target.hidden=true;try{var response=await fetch(window.__sellerAdminConfig.action_url,{method:'POST',credentials:'same-origin',headers:{Accept:'application/json','Content-Type':'application/json'},body:'{}'}),text=await response.text(),payload={};if(text){try{payload=JSON.parse(text)}catch(_error){payload={message:text}}}if(!response.ok)throw new Error(payload.message||payload.error||('Request failed ('+response.status+')'));window.location.reload()}catch(error){target.textContent=error.message;target.hidden=false;button.disabled=false;button.textContent=original}}
