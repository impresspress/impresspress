function orderDetailError(message){var target=document.getElementById('order-detail-error');if(target){target.textContent=message||'Something went wrong.';target.hidden=false;target.scrollIntoView({block:'nearest'})}}
function parseOrderRefundMinor(value,exponent){value=value.trim();if(!value)return null;if(!/^[+]?(?:\d+(?:\.\d*)?|\.\d+)$/.test(value))throw new Error('Enter a plain positive amount.');value=value.replace(/^\+/,'');var parts=value.split('.'),whole=parts[0]||'0',fraction=parts[1]||'';if(fraction.length>exponent&&/[1-9]/.test(fraction.slice(exponent)))throw new Error('The amount has too many decimal places for this currency.');fraction=fraction.slice(0,exponent).padEnd(exponent,'0');var minor=BigInt(whole)*(10n**BigInt(exponent))+BigInt(fraction||'0');if(minor<=0n)throw new Error('Refund amount must be positive.');if(minor>BigInt(Number.MAX_SAFE_INTEGER))throw new Error('This amount is too large for the browser refund form.');return Number(minor)}
// The amount as the order page writes money (`pages::display_money`, which is
// `money::format_amount_minor` then the currency): "1232567.89 NZD".
function formatOrderMoney(minor,exponent,currency){var digits=String(minor);if(exponent>0){digits=digits.padStart(exponent+1,'0');digits=digits.slice(0,-exponent)+'.'+digits.slice(-exponent)}return digits+' '+currency}
// A refund cannot be undone, and a blank amount refunds everything left, so it
// is asked in the shared confirm dialog first, in danger, with the amount it
// will send: the one typed, or the remaining balance a blank one means. A
// blank amount sends that stated figure, not "whatever remains", so a page
// left open while another refund landed is refused ("exceeds the remaining
// refundable amount") rather than refunding a figure nobody confirmed. An
// amount that does not parse is reported without asking.
async function submitOrderRefund(button){var config=window.__orderDetailConfig,target=document.getElementById('order-detail-error'),amount;if(target)target.hidden=true;try{amount=parseOrderRefundMinor(document.getElementById('order-refund-amount').value,config.currency_exponent)}catch(error){orderDetailError(error.message);return}var question=amount===null?'Refund the full remaining balance, '+formatOrderMoney(config.remaining_minor,config.currency_exponent,config.currency)+', on this order? This can\'t be undone.':'Refund '+formatOrderMoney(amount,config.currency_exponent,config.currency)+' on this order? This can\'t be undone.';if(!(await window.impresspressConfirm({question:question,label:'Create refund',tone:'danger',opener:button})))return;button.disabled=true;button.textContent='Refunding…';try{var note=document.getElementById('order-refund-note').value.trim(),body={note:note,idempotency_key:'ui_'+config.refunded_total+'_'+(amount===null?'full':amount)};body.amount_minor=amount===null?config.remaining_minor:amount;var response=await fetch(config.refund_url,{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json','Accept':'application/json'},body:JSON.stringify(body)}),payload={};try{payload=await response.json()}catch(_error){}if(!response.ok)throw new Error(payload.message||payload.error||'Refund failed.');window.location.reload()}catch(error){orderDetailError(error.message);button.disabled=false;button.textContent='Create refund'}}
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
