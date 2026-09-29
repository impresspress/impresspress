function commercePortalError(message){
  var el=document.getElementById('commerce-portal-error');
  if(el){el.textContent=message||'Something went wrong. Please try again.';el.hidden=false}
}
async function commercePortalRedirect(path,body){
  var response=await fetch(path,{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json'},body:JSON.stringify(body||{})});
  var data={};try{data=await response.json()}catch(_){}
  if(!response.ok)throw new Error(data.message||'The request could not be completed.');
  if(!data.url||!/^https:\/\//.test(data.url))throw new Error('The payment provider returned an invalid redirect.');
  window.location.assign(data.url);
}
async function startSellerOnboarding(){
  try{
    var target=window.location.origin+'/b/products/';
    await commercePortalRedirect('/b/products/api/seller/onboarding',{return_url:target+'?stripe=returned',refresh_url:target+'?stripe=refresh'});
  }catch(error){commercePortalError(error.message)}
}
async function openSellerDashboard(){
  try{await commercePortalRedirect('/b/products/api/seller/dashboard',{})}
  catch(error){commercePortalError(error.message)}
}
async function manageBuyerBilling(){
  try{await commercePortalRedirect('/b/products/billing-portal',{return_url:window.location.origin+'/b/products/'})}
  catch(error){commercePortalError(error.message)}
}
// `pp-order-billing` is handled by products-order-detail.js, which is loaded after
// this file on the one page that needs it.
// Guarded against htmx re-execution; see products-seller-admin.js.
(function(){
  if(window.__commercePortalDelegated)return;
  window.__commercePortalDelegated=true;
  document.addEventListener('click',function(e){
    if(!(e.target instanceof Element))return;
    var el=e.target.closest('[data-action]');
    if(!el)return;
    var action=el.getAttribute('data-action');
    if(action==='pp-seller-onboarding')startSellerOnboarding();
    else if(action==='pp-seller-dashboard')openSellerDashboard();
    else if(action==='pp-buyer-billing')manageBuyerBilling();
  });
})();
