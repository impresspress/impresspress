function productCatalogById(id){return document.getElementById(id)}
function productCatalogError(message,focus){var target=productCatalogById('catalog-admin-error');target.textContent=message||'Something went wrong.';target.hidden=false;if(focus&&typeof focus.focus==='function')focus.focus();target.scrollIntoView({block:'nearest'})}
function productCatalogClearError(){var target=productCatalogById('catalog-admin-error');if(target){target.textContent='';target.hidden=true}}
function productCatalogBusy(button,busy){if(!button)return;button.disabled=busy;if(busy){button.dataset.originalText=button.textContent;button.textContent='Saving…'}else if(button.dataset.originalText){button.textContent=button.dataset.originalText;delete button.dataset.originalText}}
async function productCatalogRequest(url,method,body){var response=await fetch(url,{method:method,credentials:'same-origin',headers:{Accept:'application/json','Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)}),text=await response.text(),payload={};if(text){try{payload=JSON.parse(text)}catch(_error){payload={message:text}}}if(!response.ok)throw new Error(payload.message||payload.error||('Request failed ('+response.status+')'));return payload}
function productCatalogClose(){var editor=productCatalogById('group-editor');if(editor)editor.hidden=true;productCatalogClearError()}
function productCatalogNew(){productCatalogClearError();var editor=productCatalogById('group-editor');editor.hidden=false;productCatalogById('group-editor-id').value='';productCatalogById('group-editor-name').value='';productCatalogById('group-editor-description').value='';productCatalogById('group-editor-status').value='active';productCatalogById('group-editor-title').textContent='New group';editor.scrollIntoView({block:'start'});productCatalogById('group-editor-name').focus()}
function productCatalogEditGroup(button){productCatalogNew();productCatalogById('group-editor-title').textContent='Edit group';productCatalogById('group-editor-id').value=button.dataset.recordId;productCatalogById('group-editor-name').value=button.dataset.recordName||'';productCatalogById('group-editor-description').value=button.dataset.recordDescription||'';productCatalogById('group-editor-status').value=button.dataset.recordStatus||'active'}
async function productCatalogSaveGroup(event){event.preventDefault();productCatalogClearError();var form=event.target,name=productCatalogById('group-editor-name'),button=form.querySelector('button[type="submit"]');if(!form.checkValidity()){productCatalogError('Enter a group name before saving.',name);return}productCatalogBusy(button,true);try{var id=productCatalogById('group-editor-id').value,url='/b/products/api/admin/groups'+(id?'/'+encodeURIComponent(id):'');await productCatalogRequest(url,id?'PATCH':'POST',{name:name.value.trim(),description:productCatalogById('group-editor-description').value.trim(),status:productCatalogById('group-editor-status').value});window.location.reload()}catch(error){productCatalogError(error.message);productCatalogBusy(button,false)}}
async function productCatalogDelete(button){if(!window.confirm('Delete group '+(button.dataset.recordName||'')+'? Products already using it may prevent deletion.'))return;productCatalogClearError();button.disabled=true;try{await productCatalogRequest('/b/products/api/admin/groups/'+encodeURIComponent(button.dataset.recordId),'DELETE');window.location.reload()}catch(error){productCatalogError(error.message);button.disabled=false}}
// Guarded against htmx re-execution; see products-seller-admin.js. This is the page the
// duplicate-listener defect was concrete on: Groups, Orders, Groups again used
// to leave `pc-delete` bound twice, so one click raised two confirmations and
// issued two DELETEs, the second answering not found.
(function(){
  if(window.__productCatalogDelegated)return;
  window.__productCatalogDelegated=true;
  document.addEventListener('submit',function(e){
    if(e.target instanceof Element&&e.target.getAttribute('data-action')==='pc-save-group')productCatalogSaveGroup(e);
  });
  document.addEventListener('click',function(e){
    if(!(e.target instanceof Element))return;
    var el=e.target.closest('[data-action]');
    if(!el)return;
    var action=el.getAttribute('data-action');
    if(action==='pc-new')productCatalogNew();
    else if(action==='pc-close')productCatalogClose();
    else if(action==='pc-edit-group')productCatalogEditGroup(el);
    else if(action==='pc-delete')productCatalogDelete(el);
  });
})();
