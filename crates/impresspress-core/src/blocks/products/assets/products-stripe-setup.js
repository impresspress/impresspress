async function testStripeConnection(){
  var button=document.getElementById('stripe-test-button');
  var state=document.getElementById('stripe-state');
  var error=document.getElementById('stripe-error');
  button.disabled=true;button.textContent='Testing…';error.textContent='';
  try{
    var response=await fetch('/b/products/api/admin/stripe/status',{credentials:'same-origin'});
    var data=await response.json();
    if(!response.ok)throw new Error(data.message||'Stripe connection test failed.');
    var labels={not_configured:'Not configured',connected_test:'Connected — test mode',connected_live:'Connected — live mode',misconfigured:'Connection problem'};
    state.textContent=labels[data.state]||data.state||'Unknown';
    state.className='badge '+(data.state==='connected_live'?'badge-success':data.state==='connected_test'?'badge-info':data.state==='misconfigured'?'badge-danger':'badge-warning');
    error.textContent=data.error||'Connection test completed.';
  }catch(err){error.textContent=err.message||'Stripe connection test failed.'}
  finally{button.disabled=false;button.textContent='Test connection'}
}
function stripeWebhookElement(tag,text,className){
  var element=document.createElement(tag);
  if(text!==undefined)element.textContent=text;
  if(className)element.className=className;
  return element;
}
function stripeWebhookDate(value){
  if(!value)return '—';
  var date=new Date(value);
  return Number.isNaN(date.getTime())?value:date.toLocaleString();
}
function stripeWebhookStatus(status){
  return (status||'unknown').replace(/_/g,' ');
}
function renderStripeWebhookEvents(data){
  var target=document.getElementById('stripe-webhook-events');
  var summary=document.getElementById('stripe-webhook-summary');
  var records=Array.isArray(data.records)?data.records:[];
  target.replaceChildren();
  summary.textContent=(data.total_count||0)+' event'+(data.total_count===1?'':'s')+' match this filter.';
  if(!records.length){
    target.appendChild(stripeWebhookElement('p','No matching webhook events.','text-muted text-sm'));
    return;
  }
  var table=stripeWebhookElement('table',undefined,'data-table');
  var head=document.createElement('thead'),headRow=document.createElement('tr');
  ['Event','Status','Mode / attempts','Last result','Action'].forEach(function(label){headRow.appendChild(stripeWebhookElement('th',label))});
  head.appendChild(headRow);table.appendChild(head);
  var body=document.createElement('tbody');
  records.forEach(function(event){
    var row=document.createElement('tr');
    var eventCell=document.createElement('td');
    eventCell.dataset.label='Event';
    eventCell.appendChild(stripeWebhookElement('strong',event.event_type||'Unknown event'));
    eventCell.appendChild(document.createElement('br'));
    eventCell.appendChild(stripeWebhookElement('code',event.id));
    if(event.stripe_account_id){eventCell.appendChild(document.createElement('br'));eventCell.appendChild(stripeWebhookElement('span',event.stripe_account_id,'text-muted text-sm'))}
    row.appendChild(eventCell);
    var statusCell=document.createElement('td');statusCell.dataset.label='Status';
    var badgeClass=event.status==='processed'?'badge-success':event.status==='dead_letter'?'badge-danger':event.status==='failed'?'badge-warning':'badge-info';
    statusCell.appendChild(stripeWebhookElement('span',stripeWebhookStatus(event.status),'badge '+badgeClass));row.appendChild(statusCell);
    var attempts=document.createElement('td');attempts.dataset.label='Mode / attempts';attempts.textContent=(event.livemode?'Live':'Test')+' · '+event.attempts;row.appendChild(attempts);
    var result=document.createElement('td');result.dataset.label='Last result';
    result.appendChild(stripeWebhookElement('span',event.last_error||'No processing error recorded.',event.last_error?'':'text-muted'));
    result.appendChild(document.createElement('br'));
    result.appendChild(stripeWebhookElement('span',event.next_retry_at?'Retry '+stripeWebhookDate(event.next_retry_at):stripeWebhookDate(event.updated_at),'text-muted text-sm'));
    row.appendChild(result);
    var action=document.createElement('td');action.dataset.label='Action';
    if(event.status==='failed'||event.status==='dead_letter'){
      var replay=stripeWebhookElement('button','Replay','btn btn--secondary btn--sm');replay.type='button';
      replay.setAttribute('aria-label','Replay webhook '+event.id);
      replay.onclick=function(){replayStripeWebhookEvent(event.id,replay)};action.appendChild(replay);
    }else{action.appendChild(stripeWebhookElement('span','—','text-muted'))}
    row.appendChild(action);body.appendChild(row);
  });
  table.appendChild(body);target.appendChild(table);
}
// Both lists are replaced wholesale, so two loads in flight at once can land
// out of order and leave the list disagreeing with the filter that produced
// it. Each load takes a ticket and only paints if it is still the newest --
// last request wins, and a superseded response is dropped rather than blocked,
// which a plain busy flag could not do without losing the newer filter value.
var stripeWebhookLoadTicket=0;
async function loadStripeWebhookEvents(){
  var ticket=++stripeWebhookLoadTicket;
  var target=document.getElementById('stripe-webhook-events');
  var error=document.getElementById('stripe-webhook-error');
  var status=document.getElementById('stripe-webhook-filter').value;
  error.hidden=true;error.textContent='';target.textContent='Loading webhook events…';
  try{
    var query='?page=1&page_size=50'+(status?'&status='+encodeURIComponent(status):'');
    var response=await fetch('/b/products/api/admin/webhook-events'+query,{credentials:'same-origin'});
    var data={};try{data=await response.json()}catch(_){}
    if(ticket!==stripeWebhookLoadTicket)return;
    if(!response.ok)throw new Error(data.message||'Could not load webhook events.');
    renderStripeWebhookEvents(data);
  }catch(err){
    if(ticket!==stripeWebhookLoadTicket)return;
    target.replaceChildren();error.textContent=err.message||'Could not load webhook events.';error.hidden=false
  }
}
async function replayStripeWebhookEvent(id,button){
  if(!window.confirm('Replay this Stripe webhook through the normal validation pipeline?'))return;
  var error=document.getElementById('stripe-webhook-error');
  button.disabled=true;button.textContent='Replaying…';error.hidden=true;
  try{
    var response=await fetch('/b/products/api/admin/webhook-events/'+encodeURIComponent(id)+'/replay',{method:'POST',credentials:'same-origin'});
    var data={};try{data=await response.json()}catch(_){}
    if(!response.ok)throw new Error(data.message||'Could not replay the webhook event.');
    await loadStripeWebhookEvents();
  }catch(err){error.textContent=err.message||'Could not replay the webhook event.';error.hidden=false;button.disabled=false;button.textContent='Replay'}
}
function renderStripeProviderOperations(data){
  var target=document.getElementById('stripe-provider-operations-list');
  var summary=document.getElementById('stripe-provider-summary');
  var records=Array.isArray(data.records)?data.records:[];
  target.replaceChildren();
  summary.textContent=(data.total_count||0)+' operation'+(data.total_count===1?'':'s')+' match this filter.';
  if(!records.length){target.appendChild(stripeWebhookElement('p','No matching provider operations.','text-muted text-sm'));return}
  var table=stripeWebhookElement('table',undefined,'data-table');
  var head=document.createElement('thead'),headRow=document.createElement('tr');
  ['Operation','Status','Attempts','Last result'].forEach(function(label){headRow.appendChild(stripeWebhookElement('th',label))});
  head.appendChild(headRow);table.appendChild(head);var body=document.createElement('tbody');
  records.forEach(function(operation){
    var row=document.createElement('tr');
    var identity=document.createElement('td');identity.dataset.label='Operation';
    identity.appendChild(stripeWebhookElement('strong',operation.operation_type||'Provider operation'));
    identity.appendChild(document.createElement('br'));identity.appendChild(stripeWebhookElement('code',operation.aggregate_id||operation.id));
    if(operation.stripe_account_id){identity.appendChild(document.createElement('br'));identity.appendChild(stripeWebhookElement('span',operation.stripe_account_id,'text-muted text-sm'))}
    row.appendChild(identity);
    var state=document.createElement('td');state.dataset.label='Status';
    var badgeClass=operation.status==='succeeded'?'badge-success':operation.status==='dead_letter'?'badge-danger':operation.status==='failed'?'badge-warning':'badge-info';
    state.appendChild(stripeWebhookElement('span',stripeWebhookStatus(operation.status),'badge '+badgeClass));row.appendChild(state);
    var attempts=document.createElement('td');attempts.dataset.label='Attempts';attempts.textContent=String(operation.attempts||0);row.appendChild(attempts);
    var result=document.createElement('td');result.dataset.label='Last result';
    result.appendChild(stripeWebhookElement('span',operation.last_error||'No reconciliation error recorded.',operation.last_error?'':'text-muted'));
    result.appendChild(document.createElement('br'));
    result.appendChild(stripeWebhookElement('span',operation.next_attempt_at?'Retry '+stripeWebhookDate(operation.next_attempt_at):stripeWebhookDate(operation.updated_at),'text-muted text-sm'));
    row.appendChild(result);body.appendChild(row);
  });
  table.appendChild(body);target.appendChild(table);
}
// Ticketed for the same reason as loadStripeWebhookEvents above.
var stripeProviderLoadTicket=0;
async function loadStripeProviderOperations(){
  var ticket=++stripeProviderLoadTicket;
  var target=document.getElementById('stripe-provider-operations-list');
  var error=document.getElementById('stripe-provider-error');
  var status=document.getElementById('stripe-provider-filter').value;
  error.hidden=true;error.textContent='';target.textContent='Loading provider operations…';
  try{
    var query='?page=1&page_size=50'+(status?'&status='+encodeURIComponent(status):'');
    var response=await fetch('/b/products/api/admin/provider-operations'+query,{credentials:'same-origin'});
    var data={};try{data=await response.json()}catch(_){}
    if(ticket!==stripeProviderLoadTicket)return;
    if(!response.ok)throw new Error(data.message||'Could not load provider operations.');
    renderStripeProviderOperations(data);
  }catch(err){
    if(ticket!==stripeProviderLoadTicket)return;
    target.replaceChildren();error.textContent=err.message||'Could not load provider operations.';error.hidden=false
  }
}
async function reconcileStripeProviderOperations(button){
  var error=document.getElementById('stripe-provider-error');
  var result=document.getElementById('stripe-provider-reconcile-result');
  button.disabled=true;button.textContent='Reconciling…';error.hidden=true;result.textContent='';
  try{
    var response=await fetch('/b/products/api/admin/provider-operations/reconcile?limit=50',{method:'POST',credentials:'same-origin'});
    var data={};try{data=await response.json()}catch(_){}
    if(!response.ok)throw new Error(data.message||'Could not reconcile provider operations.');
    result.textContent='Claimed '+data.claimed+'; completed '+data.succeeded+'; retry scheduled '+data.retry_scheduled+'; manual review '+data.dead_letter+'.'+(data.unrecorded>0?' '+data.unrecorded+' could not be recorded and will be retried; see the server log.':'');
    await loadStripeProviderOperations();
  }catch(err){error.textContent=err.message||'Could not reconcile provider operations.';error.hidden=false}
  finally{button.disabled=false;button.textContent='Reconcile due operations'}
}
// Guarded against htmx re-execution; see products-seller-admin.js. The two initial
// loads below stay outside the guard: the swap brought in empty containers, so
// they have to be filled again even though the listeners are already bound.
(function(){
  if(window.__stripeSetupDelegated)return;
  window.__stripeSetupDelegated=true;
  document.addEventListener('click',function(e){
    if(!(e.target instanceof Element))return;
    var el=e.target.closest('[data-action]');
    if(!el)return;
    var action=el.getAttribute('data-action');
    if(action==='ps-test-connection')testStripeConnection();
    else if(action==='ps-reconcile')reconcileStripeProviderOperations(el);
    // The two filter <select>s carry the same verbs as their Refresh buttons,
    // and `closest('[data-action]')` matches the <select> itself -- so without
    // this the mousedown that OPENS the dropdown would fire a load with the
    // value the user is on their way to changing, and the change event would
    // fire a second one. Only the buttons act on click.
    else if(el.tagName!=='SELECT'){
      if(action==='ps-load-webhooks')loadStripeWebhookEvents();
      else if(action==='ps-load-provider-ops')loadStripeProviderOperations();
    }
  });
  document.addEventListener('change',function(e){
    var el=e.target;
    if(!(el instanceof Element))return;
    var action=el.getAttribute('data-action');
    if(action==='ps-load-webhooks')loadStripeWebhookEvents();
    else if(action==='ps-load-provider-ops')loadStripeProviderOperations();
  });
})();
loadStripeWebhookEvents();
loadStripeProviderOperations();
