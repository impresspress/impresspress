function orderDetailError(message){var target=document.getElementById('order-detail-error');if(target){target.textContent=message||'Something went wrong.';target.hidden=false;target.scrollIntoView({block:'nearest'})}}
function parseOrderRefundMinor(value,exponent){value=value.trim();if(!value)return null;if(!/^[+]?(?:\d+(?:\.\d*)?|\.\d+)$/.test(value))throw new Error('Enter a plain positive amount.');value=value.replace(/^\+/,'');var parts=value.split('.'),whole=parts[0]||'0',fraction=parts[1]||'';if(fraction.length>exponent&&/[1-9]/.test(fraction.slice(exponent)))throw new Error('The amount has too many decimal places for this currency.');fraction=fraction.slice(0,exponent).padEnd(exponent,'0');var minor=BigInt(whole)*(10n**BigInt(exponent))+BigInt(fraction||'0');if(minor<=0n)throw new Error('Refund amount must be positive.');if(minor>BigInt(Number.MAX_SAFE_INTEGER))throw new Error('This amount is too large for the browser refund form.');return Number(minor)}
async function submitOrderRefund(button){var config=window.__orderDetailConfig,target=document.getElementById('order-detail-error');if(target)target.hidden=true;button.disabled=true;button.textContent='Refunding…';try{var amount=parseOrderRefundMinor(document.getElementById('order-refund-amount').value,config.currency_exponent),note=document.getElementById('order-refund-note').value.trim(),body={note:note,idempotency_key:'ui_'+config.refunded_total+'_'+(amount===null?'full':amount)};if(amount!==null)body.amount_minor=amount;var response=await fetch(config.refund_url,{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json','Accept':'application/json'},body:JSON.stringify(body)}),payload={};try{payload=await response.json()}catch(_error){}if(!response.ok)throw new Error(payload.message||payload.error||'Refund failed.');window.location.reload()}catch(error){orderDetailError(error.message);button.disabled=false;button.textContent='Create refund'}}
async function manageOrderBilling(){var config=window.__orderDetailConfig;try{await commercePortalRedirect('/b/products/billing-portal',{return_url:window.location.href,order_id:config.order_id})}catch(error){orderDetailError(error.message)}}
// Guarded against htmx re-execution; see products-seller-admin.js. The refund is the
// one mutating action on these pages that carries an idempotency key, so a
// double dispatch would not have double-charged -- but it would still have
// raised two requests and two error paints.
(function(){
  if(window.__orderDetailDelegated)return;
  window.__orderDetailDelegated=true;
  document.addEventListener('click',function(e){
    if(!(e.target instanceof Element))return;
    var el=e.target.closest('[data-action]');
    if(!el)return;
    var action=el.getAttribute('data-action');
    if(action==='po-submit-refund')submitOrderRefund(el);
    else if(action==='pp-order-billing')manageOrderBilling();
  });
})();
