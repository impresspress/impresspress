var productWizardStep=1;
var productWizardVariableIndex=0;
var productWizardComponentIndex=0;

function wizardById(id){return document.getElementById(id)}
function productWizardTemplate(){
  var selected=document.querySelector('input[name="product_template"]:checked');
  return selected?selected.value:'simple_product';
}
function productWizardIsSubscription(){return productWizardTemplate().indexOf('subscription')!==-1}
function productWizardIsConfigurable(){return productWizardTemplate().indexOf('configurable')===0}
function productWizardShowError(message,focus){
  var error=wizardById('product-wizard-error');
  error.textContent=message;error.hidden=false;
  if(focus&&typeof focus.focus==='function')focus.focus();
  error.scrollIntoView({block:'center'});
}
function productWizardClearError(){var error=wizardById('product-wizard-error');error.textContent='';error.hidden=true}
function productWizardSlug(value){
  return value.toLowerCase().normalize('NFKD').replace(/[\u0300-\u036f]/g,'').replace(/[^a-z0-9]+/g,'-').replace(/^-+|-+$/g,'').slice(0,160).replace(/-+$/,'');
}
function productWizardTemplateChanged(){
  var subscription=productWizardIsSubscription();
  var configurable=productWizardIsConfigurable();
  document.querySelectorAll('[data-subscription-field]').forEach(function(el){el.hidden=!subscription});
  document.querySelectorAll('[data-simple-pricing]').forEach(function(el){el.hidden=configurable});
  wizardById('wizard-advanced-pricing').hidden=!configurable;
  if(configurable&&wizardById('wizard-variables').children.length===0){
    addWizardVariable({key:'quantity',label:'Quantity',kind:'integer',required:true,minimum:'1',maximum:'100',step:'1'});
    addWizardComponent({key:'base',label:'Base price',amount_type:'fixed',amount:'0.00',required:true});
    addWizardComponent({key:'quantity',label:'Quantity',amount_type:'per_unit',amount:'0.00',input:'quantity',required:true});
  }
}
function productWizardShowStep(step,scrollToStep){
  productWizardStep=Math.max(1,Math.min(5,step));
  document.querySelectorAll('[data-wizard-step]').forEach(function(el){el.hidden=Number(el.dataset.wizardStep)!==productWizardStep});
  document.querySelectorAll('[data-wizard-indicator]').forEach(function(el){
    var current=Number(el.dataset.wizardIndicator);
    el.className='badge '+(current===productWizardStep?'badge-primary':current<productWizardStep?'badge-success':'badge-secondary');
    var check=el.querySelector('.wizard-step-check');
    if(check)check.hidden=current>=productWizardStep;
  });
  wizardById('wizard-previous').hidden=productWizardStep===1;
  wizardById('wizard-next').hidden=productWizardStep===5;
  wizardById('wizard-save-draft').hidden=productWizardStep!==5;
  wizardById('wizard-publish').hidden=productWizardStep!==5;
  if(productWizardStep===5)renderProductWizardReview();
  productWizardClearError();
  var current=document.querySelector('[data-wizard-step="'+productWizardStep+'"]');
  if(current&&scrollToStep!==false)current.scrollIntoView({block:'start'});
}
function productWizardValidateStep(step){
  if(step===2){
    var name=wizardById('wizard-name');
    if(!name.value.trim()){productWizardShowError('Product name is required.',name);return false}
    var slug=wizardById('wizard-slug');
    if(slug.value.trim()&&!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(slug.value.trim())){
      productWizardShowError('URL slug may contain lowercase letters, numbers, and single hyphens.',slug);return false
    }
    var image=wizardById('wizard-image');
    if(image.value&&!image.checkValidity()){productWizardShowError('Image URL must be a valid absolute URL.',image);return false}
  }
  if(step===3){
    try{buildProductWizardOffer()}catch(error){productWizardShowError(error.message);return false}
  }
  return true;
}
function productWizardNext(){if(productWizardValidateStep(productWizardStep))productWizardShowStep(productWizardStep+1)}
function productWizardPrevious(){productWizardShowStep(productWizardStep-1)}

function addWizardVariable(seed){
  seed=seed||{};var index=productWizardVariableIndex++;
  var row=document.createElement('section');row.className='card mt-3';row.dataset.variableRow='';
  row.innerHTML=`<div class="card__body">
    <div class="flex justify-between gap-3 items-center"><strong>Customer input</strong><button class="btn btn--secondary btn--sm" type="button" data-remove-row>Remove</button></div>
    <div class="grid grid-auto-150 gap-3 mt-3">
      <div class="form-group"><label class="form-label required" for="wizard-variable-key-${index}">Key</label><input class="form-input" id="wizard-variable-key-${index}" data-variable-key required placeholder="quantity"></div>
      <div class="form-group"><label class="form-label required" for="wizard-variable-label-${index}">Label</label><input class="form-input" id="wizard-variable-label-${index}" data-variable-label required placeholder="Quantity"></div>
      <div class="form-group"><label class="form-label" for="wizard-variable-kind-${index}">Type</label><select class="form-select" id="wizard-variable-kind-${index}" data-variable-kind><option value="integer">Whole number</option><option value="number">Decimal number</option><option value="date">Date</option><option value="date_time">Date and time</option><option value="boolean">Yes / no</option><option value="select">Choice</option><option value="multi_select">Multiple choices</option><option value="text">Text</option></select></div>
      <div class="form-group" data-variable-min-wrap><label class="form-label" for="wizard-variable-min-${index}">Minimum</label><input class="form-input" id="wizard-variable-min-${index}" data-variable-min inputmode="decimal"></div>
      <div class="form-group" data-variable-max-wrap><label class="form-label" for="wizard-variable-max-${index}">Maximum</label><input class="form-input" id="wizard-variable-max-${index}" data-variable-max inputmode="decimal"></div>
      <div class="form-group" data-variable-step-wrap><label class="form-label" for="wizard-variable-step-${index}">Step</label><input class="form-input" id="wizard-variable-step-${index}" data-variable-step inputmode="decimal"></div>
      <div class="form-group" data-variable-options-wrap><label class="form-label" for="wizard-variable-options-${index}">Choices</label><input class="form-input" id="wizard-variable-options-${index}" data-variable-options placeholder="small, medium, large"></div>
      <div class="form-group"><label class="form-label" for="wizard-variable-visibility-${index}">Visibility</label><select class="form-select" id="wizard-variable-visibility-${index}" data-variable-visibility><option value="public">Customer</option><option value="hidden">Hidden</option><option value="admin_only">Admin only</option></select></div>
      <div class="form-group"><label class="form-label" for="wizard-variable-default-${index}">Default value</label><input class="form-input" id="wizard-variable-default-${index}" data-variable-default placeholder="Optional"></div>
      <div class="form-group" data-variable-length-wrap><label class="form-label" for="wizard-variable-length-${index}">Maximum text length</label><input class="form-input" id="wizard-variable-length-${index}" data-variable-length type="number" min="1" max="10000"></div>
    </div><label class="flex gap-2"><input type="checkbox" data-variable-required> Required</label>
    <div class="form-group"><label class="form-label" for="wizard-variable-help-${index}">Help text</label><input class="form-input" id="wizard-variable-help-${index}" data-variable-help maxlength="500" placeholder="Shown beside this input"></div>
  </div>`;
  row.querySelector('[data-remove-row]').onclick=function(){row.remove()};
  row.querySelector('[data-variable-key]').value=seed.key||'';
  row.querySelector('[data-variable-label]').value=seed.label||'';
  row.querySelector('[data-variable-kind]').value=seed.kind||'integer';
  row.querySelector('[data-variable-min]').value=seed.minimum||'';
  row.querySelector('[data-variable-max]').value=seed.maximum||'';
  row.querySelector('[data-variable-step]').value=seed.step||'';
  row.querySelector('[data-variable-options]').value=(seed.allowed_values||[]).join(', ');
  row.querySelector('[data-variable-visibility]').value=seed.visibility||'public';
  row.querySelector('[data-variable-default]').value=seed.default_value===undefined||seed.default_value===null?'':Array.isArray(seed.default_value)?seed.default_value.join(', '):String(seed.default_value);
  row.querySelector('[data-variable-length]').value=seed.maximum_length||'';
  row.querySelector('[data-variable-help]').value=seed.help_text||'';
  row.querySelector('[data-variable-required]').checked=seed.required!==false;
  row.querySelector('[data-variable-kind]').onchange=function(){wizardVariableKindChanged(row)};
  wizardVariableKindChanged(row);
  wizardById('wizard-variables').appendChild(row);
}

function wizardVariableKindChanged(row){
  var kind=row.querySelector('[data-variable-kind]').value,numeric=kind==='integer'||kind==='number',dated=kind==='date'||kind==='date_time',choices=kind==='select'||kind==='multi_select';
  var minimum=row.querySelector('[data-variable-min]'),maximum=row.querySelector('[data-variable-max]'),defaultInput=row.querySelector('[data-variable-default]');
  row.querySelector('[data-variable-min-wrap]').hidden=!(numeric||dated);row.querySelector('[data-variable-max-wrap]').hidden=!(numeric||dated);row.querySelector('[data-variable-step-wrap]').hidden=!numeric;row.querySelector('[data-variable-options-wrap]').hidden=!choices;row.querySelector('[data-variable-length-wrap]').hidden=kind!=='text';
  minimum.type=kind==='date'?'date':kind==='date_time'?'datetime-local':'text';maximum.type=minimum.type;defaultInput.type=minimum.type;
  minimum.inputMode=numeric?'decimal':'';maximum.inputMode=numeric?'decimal':'';
}

function addWizardComponent(seed){
  seed=seed||{};var index=productWizardComponentIndex++;
  var row=document.createElement('section');row.className='card mt-3';row.dataset.componentRow='';
  row.innerHTML=`<div class="card__body">
    <div class="flex justify-between gap-3 items-center"><strong>Price row</strong><button class="btn btn--secondary btn--sm" type="button" data-remove-row>Remove</button></div>
    <div class="grid grid-auto-150 gap-3 mt-3">
      <div class="form-group"><label class="form-label required" for="wizard-component-key-${index}">Key</label><input class="form-input" id="wizard-component-key-${index}" data-component-key required placeholder="base"></div>
      <div class="form-group"><label class="form-label required" for="wizard-component-label-${index}">Label</label><input class="form-input" id="wizard-component-label-${index}" data-component-label required placeholder="Base price"></div>
      <div class="form-group"><label class="form-label" for="wizard-component-description-${index}">Description</label><input class="form-input" id="wizard-component-description-${index}" data-component-description maxlength="500"></div>
      <div class="form-group"><label class="form-label" for="wizard-component-type-${index}">Calculation</label><select class="form-select" id="wizard-component-type-${index}" data-component-type><option value="fixed">Fixed amount</option><option value="per_unit">Amount × input</option><option value="flat_plus_per_unit">Base + amount × input</option><option value="lookup">Price selected by input</option><option value="graduated">Graduated tiers</option><option value="volume">Volume tiers</option><option value="package">Packages / blocks</option></select></div>
      <div class="form-group"><label class="form-label required" for="wizard-component-amount-${index}">Amount / unit rate</label><input class="form-input" id="wizard-component-amount-${index}" data-component-amount inputmode="decimal" value="0.00" required></div>
      <div class="form-group"><label class="form-label" for="wizard-component-input-${index}">Pricing input key</label><input class="form-input" id="wizard-component-input-${index}" data-component-input placeholder="quantity"></div>
      <div class="form-group"><label class="form-label" for="wizard-condition-${index}">Condition</label><select class="form-select" id="wizard-condition-${index}" data-component-condition><option value="always">Always include</option><option value="equals">Input equals value</option><option value="not_equals">Input does not equal value</option><option value="greater_than">Input is greater than value</option><option value="greater_than_or_equal">Input is at least value</option><option value="less_than">Input is less than value</option><option value="less_than_or_equal">Input is at most value</option><option value="contains">Input contains value</option><option value="in">Input is one of these values</option><option value="present">Input is present</option><option value="advanced_preserved" hidden>Advanced condition (preserved)</option></select></div>
      <div class="form-group"><label class="form-label" for="wizard-condition-input-${index}">Condition input</label><input class="form-input" id="wizard-condition-input-${index}" data-condition-input></div>
      <div class="form-group"><label class="form-label" for="wizard-condition-value-${index}">Condition value</label><input class="form-input" id="wizard-condition-value-${index}" data-condition-value></div>
    </div>
    <details class="my-3"><summary>Advanced calculation details</summary>
      <div class="grid grid-auto-180 gap-3 mt-3">
        <div class="form-group"><label class="form-label" for="wizard-component-base-${index}">Base amount</label><input class="form-input" id="wizard-component-base-${index}" data-component-base inputmode="decimal" value="0.00"><p class="text-muted text-sm">Used by base + per-unit pricing.</p></div>
        <div class="form-group"><label class="form-label" for="wizard-component-package-size-${index}">Units per package</label><input class="form-input" id="wizard-component-package-size-${index}" data-component-package-size type="number" min="1" value="1"><p class="text-muted text-sm">Used by package pricing.</p></div>
        <div class="form-group"><label class="form-label" for="wizard-component-rounding-${index}">Partial packages</label><select class="form-select" id="wizard-component-rounding-${index}" data-component-rounding><option value="up">Round up and charge a package</option><option value="exact">Require an exact multiple</option></select></div>
      </div>
      <div class="form-group"><label class="form-label" for="wizard-component-details-${index}">Lookup prices or tiers</label><textarea class="form-textarea" id="wizard-component-details-${index}" data-component-details rows="4" placeholder="Lookup: small = 10.00&#10;Tier: 10 | 1.00 | 0.00&#10;Final tier: * | 0.80 | 0.00"></textarea><p class="text-muted text-sm">Lookup rows use <code>choice = amount</code>. Tier rows use <code>upper bound | unit amount | flat amount</code>; use <code>*</code> for the final open tier.</p></div>
    </details>
    <label class="flex gap-2"><input type="checkbox" data-component-required> Required row</label>
  </div>`;
  row.querySelector('[data-remove-row]').onclick=function(){row.remove()};
  row.querySelector('[data-component-key]').value=seed.key||'';
  row.querySelector('[data-component-label]').value=seed.label||'';
  row.querySelector('[data-component-description]').value=seed.description||'';
  row.querySelector('[data-component-type]').value=seed.amount_type||'fixed';
  row.querySelector('[data-component-amount]').value=seed.amount||'0.00';
  row.querySelector('[data-component-input]').value=seed.input||'';
  row.querySelector('[data-component-base]').value=seed.base_amount||'0.00';
  row.querySelector('[data-component-package-size]').value=seed.units_per_package||'1';
  row.querySelector('[data-component-rounding]').value=seed.rounding||'up';
  row.querySelector('[data-component-details]').value=seed.details||'';
  row.querySelector('[data-component-condition]').value=seed.condition||'always';
  if(seed.preserved_condition){row.dataset.preservedCondition=JSON.stringify(seed.preserved_condition);row.querySelector('[data-component-condition]').querySelector('[value="advanced_preserved"]').hidden=false;row.querySelector('[data-component-condition]').value='advanced_preserved'}
  if(seed.preserved_quantity)row.dataset.preservedQuantity=JSON.stringify(seed.preserved_quantity);
  if(seed.preserved_metadata)row.dataset.preservedMetadata=JSON.stringify(seed.preserved_metadata);
  row.querySelector('[data-condition-input]').value=seed.condition_input||'';
  row.querySelector('[data-condition-value]').value=seed.condition_value||'';
  row.querySelector('[data-component-required]').checked=seed.required!==false;
  wizardById('wizard-components').appendChild(row);
}

function wizardCurrencyExponent(currency){
  var zero=['BIF','CLP','DJF','GNF','JPY','KMF','KRW','MGA','PYG','RWF','UGX','VND','VUV','XAF','XOF','XPF'];
  var three=['BHD','JOD','KWD','OMR','TND'];
  return zero.indexOf(currency)!==-1?0:three.indexOf(currency)!==-1?3:2;
}
function wizardMoneyToMinor(raw,currency){
  raw=String(raw).trim();currency=String(currency).trim().toUpperCase();
  if(!/^[A-Z]{3}$/.test(currency))throw new Error('Currency must be a three-letter ISO code.');
  if(!/^\+?(?:\d+(?:\.\d*)?|\.\d+)$/.test(raw))throw new Error('Amounts must be non-negative plain decimal numbers.');
  raw=raw.replace(/^\+/,'');var parts=raw.split('.');var whole=parts[0]||'0';var fraction=parts[1]||'';var exponent=wizardCurrencyExponent(currency);
  if(fraction.length>exponent&&/[^0]/.test(fraction.slice(exponent)))throw new Error('Amount has more than '+exponent+' decimal places for '+currency+'.');
  fraction=fraction.slice(0,exponent).padEnd(exponent,'0');
  var multiplier=BigInt(10)**BigInt(exponent);var minor=BigInt(whole)*multiplier+BigInt(fraction||'0');
  if(minor>BigInt(Number.MAX_SAFE_INTEGER))throw new Error('Amount is too large.');
  return Number(minor);
}
function wizardMinorToDisplay(minor,currency){
  var exponent=wizardCurrencyExponent(currency),raw=String(minor).padStart(exponent+1,'0');
  return exponent===0?raw:raw.slice(0,-exponent)+'.'+raw.slice(-exponent);
}
function collectWizardVariables(){
  var variables=[],keys=new Set();
  document.querySelectorAll('[data-variable-row]').forEach(function(row,index){
    var key=row.querySelector('[data-variable-key]').value.trim();var label=row.querySelector('[data-variable-label]').value.trim();var kind=row.querySelector('[data-variable-kind]').value;
    if(!/^[A-Za-z][A-Za-z0-9_]*$/.test(key))throw new Error('Each customer input needs a unique key using letters, numbers, and underscores.');
    if(keys.has(key))throw new Error('Customer input keys must be unique: '+key);keys.add(key);
    if(!label)throw new Error('Each customer input needs a label.');
    var variable={key:key,kind:kind,label:label,required:row.querySelector('[data-variable-required]').checked,visibility:row.querySelector('[data-variable-visibility]').value,sort_order:index};
    var minimum=row.querySelector('[data-variable-min]').value.trim(),maximum=row.querySelector('[data-variable-max]').value.trim(),step=row.querySelector('[data-variable-step]').value.trim();
    if((kind==='integer'||kind==='number'||kind==='date'||kind==='date_time')&&minimum)variable.minimum=minimum;
    if((kind==='integer'||kind==='number'||kind==='date'||kind==='date_time')&&maximum)variable.maximum=maximum;
    if((kind==='integer'||kind==='number')&&step)variable.step=step;
    if(kind==='select'||kind==='multi_select'){
      variable.allowed_values=row.querySelector('[data-variable-options]').value.split(',').map(function(v){return v.trim()}).filter(Boolean);
      if(variable.allowed_values.length===0)throw new Error('Choice input '+key+' needs at least one allowed value.');
    }
    var help=row.querySelector('[data-variable-help]').value.trim(),defaultRaw=row.querySelector('[data-variable-default]').value.trim(),maximumLength=Number(row.querySelector('[data-variable-length]').value||0);
    if(help)variable.help_text=help;
    if(maximumLength){if(!Number.isSafeInteger(maximumLength)||maximumLength<1||maximumLength>10000)throw new Error('Maximum text length on '+key+' must be between 1 and 10000.');variable.maximum_length=maximumLength}
    if(defaultRaw!==''){
      if(kind==='multi_select')variable.default_value=defaultRaw.split(',').map(function(value){return value.trim()}).filter(Boolean);
      else variable.default_value=wizardConditionValue(defaultRaw,variable);
    }
    variables.push(variable);
  });
  return variables;
}
function wizardConditionValue(raw,variable){
  if(!variable)return raw;
  if(variable.kind==='boolean'){
    if(raw!=='true'&&raw!=='false')throw new Error('Boolean conditions must use true or false.');return raw==='true';
  }
  if(variable.kind==='integer'){
    if(!/^-?\d+$/.test(raw))throw new Error('Integer condition values must be whole numbers.');return Number(raw);
  }
  if(variable.kind==='number'){
    if(!/^-?(?:\d+(?:\.\d*)?|\.\d+)$/.test(raw))throw new Error('Number condition values must be decimal numbers.');return raw;
  }
  if(variable.kind==='date'&&!/^\d{4}-\d{2}-\d{2}$/.test(raw))throw new Error('Date values must use YYYY-MM-DD.');
  if(variable.kind==='date_time'&&!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(raw))throw new Error('Date and time values must use YYYY-MM-DDTHH:MM.');
  return raw;
}
function wizardPricingLines(raw){return String(raw).split(/\r?\n/).map(function(line){return line.trim()}).filter(Boolean)}
function wizardParseLookup(raw,currency,key){
  var prices={};wizardPricingLines(raw).forEach(function(line){
    var split=line.indexOf('=');if(split<1)throw new Error('Lookup row '+key+' must use choice = amount.');
    var choice=line.slice(0,split).trim(),value=line.slice(split+1).trim();
    if(!choice||Object.prototype.hasOwnProperty.call(prices,choice))throw new Error('Lookup choices on '+key+' must be non-empty and unique.');
    prices[choice]=wizardMoneyToMinor(value,currency);
  });
  if(Object.keys(prices).length===0)throw new Error('Lookup row '+key+' needs at least one choice and amount.');return prices;
}
function wizardParseTiers(raw,currency,key){
  var tiers=wizardPricingLines(raw).map(function(line,index,lines){
    var parts=line.split('|').map(function(part){return part.trim()});
    if(parts.length<2||parts.length>3)throw new Error('Tier row '+key+' must use upper bound | unit amount | optional flat amount.');
    var open=parts[0]==='*',upTo=open?undefined:Number(parts[0]);
    if(!open&&(!Number.isSafeInteger(upTo)||upTo<1))throw new Error('Tier bounds on '+key+' must be positive whole numbers.');
    if(open&&index!==lines.length-1)throw new Error('Only the final tier on '+key+' may use *.');
    if(!open&&index===lines.length-1)throw new Error('The final tier on '+key+' must use *.');
    var tier={unit_amount_minor:wizardMoneyToMinor(parts[1],currency),flat_amount_minor:wizardMoneyToMinor(parts[2]||'0',currency)};
    if(!open)tier.up_to=upTo;return tier;
  });
  if(tiers.length===0)throw new Error('Tiered row '+key+' needs at least one tier.');
  for(var i=1;i<tiers.length;i++){if(tiers[i-1].up_to!==undefined&&tiers[i].up_to!==undefined&&tiers[i].up_to<=tiers[i-1].up_to)throw new Error('Tier bounds on '+key+' must increase.')}
  return tiers;
}
function productWizardShippingChanged(){
  var settings=wizardById('wizard-shipping-settings'),shipping=wizardById('wizard-shipping-address');
  if(settings&&shipping)settings.hidden=!shipping.checked;
}
function wizardParseShippingCountries(raw){
  var seen=new Set(),countries=String(raw).split(',').map(function(value){return value.trim().toUpperCase()}).filter(Boolean);
  if(countries.length===0)throw new Error('Add at least one allowed shipping country.');
  if(countries.length>50)throw new Error('At most 50 shipping countries may be configured.');
  countries.forEach(function(country){if(!/^[A-Z]{2}$/.test(country))throw new Error('Shipping countries must use two-letter codes.');if(seen.has(country))throw new Error('Shipping countries must be unique.');seen.add(country)});
  return countries;
}
function wizardParseShippingOptions(raw,currency,taxBehavior){
  var units=new Set(['hour','day','business_day','week','month']);
  var options=wizardPricingLines(raw).map(function(line){
    var parts=line.split('|').map(function(part){return part.trim()});
    if(parts.length<2||parts.length>6)throw new Error('Shipping options must use name | amount | minimum | maximum | unit | optional Stripe rate ID.');
    while(parts.length<6)parts.push('');
    var name=parts[0],minimum=parts[2]===''?undefined:Number(parts[2]),maximum=parts[3]===''?undefined:Number(parts[3]),unit=parts[4],stripeId=parts[5];
    if(!name||name.length>100)throw new Error('Shipping option names must contain between 1 and 100 characters.');
    if(minimum!==undefined&&(!Number.isSafeInteger(minimum)||minimum<1))throw new Error('Shipping estimate minimums must be positive whole numbers.');
    if(maximum!==undefined&&(!Number.isSafeInteger(maximum)||maximum<1))throw new Error('Shipping estimate maximums must be positive whole numbers.');
    if(minimum!==undefined&&maximum!==undefined&&minimum>maximum)throw new Error('Shipping estimate minimums must not exceed maximums.');
    if((minimum!==undefined||maximum!==undefined)&&!units.has(unit))throw new Error('Shipping estimates need a valid time unit.');
    if(minimum===undefined&&maximum===undefined&&unit!=='')throw new Error('A shipping time unit needs a minimum or maximum estimate.');
    if(stripeId&&!/^shr_[A-Za-z0-9_]+$/.test(stripeId))throw new Error('Stripe shipping rate IDs must start with shr_.');
    var option={display_name:name,amount_minor:wizardMoneyToMinor(parts[1],currency),tax_behavior:taxBehavior,stripe_shipping_rate_id:stripeId};
    if(minimum!==undefined||maximum!==undefined){option.delivery_estimate={minimum:minimum,maximum:maximum,unit:unit}}
    return option;
  });
  if(options.length>5)throw new Error('Stripe Checkout supports at most five shipping options.');
  return options;
}
function collectWizardComponents(variables,currency,subscription,interval,intervalCount){
  var components=[],keys=new Set(),byKey={};variables.forEach(function(v){byKey[v.key]=v});
  document.querySelectorAll('[data-component-row]').forEach(function(row,index){
    var key=row.querySelector('[data-component-key]').value.trim(),label=row.querySelector('[data-component-label]').value.trim();
    if(!/^[A-Za-z][A-Za-z0-9_]*$/.test(key)||keys.has(key))throw new Error('Each price row needs a unique key using letters, numbers, and underscores.');keys.add(key);
    if(!label)throw new Error('Each price row needs a label.');
    var type=row.querySelector('[data-component-type]').value,input=row.querySelector('[data-component-input]').value.trim(),amount;
    var numeric=type==='per_unit'||type==='flat_plus_per_unit'||type==='graduated'||type==='volume'||type==='package';
    if(type!=='fixed'&&!byKey[input])throw new Error('Price row '+key+' must reference an existing input.');
    if(numeric&&byKey[input].kind!=='integer'&&byKey[input].kind!=='number')throw new Error('Price row '+key+' must reference a number input.');
    if(type==='fixed')amount={type:'fixed',unit_amount_minor:wizardMoneyToMinor(row.querySelector('[data-component-amount]').value,currency)};
    else if(type==='per_unit')amount={type:'per_unit',input:input,unit_amount_minor:wizardMoneyToMinor(row.querySelector('[data-component-amount]').value,currency)};
    else if(type==='flat_plus_per_unit')amount={type:'flat_plus_per_unit',base_amount_minor:wizardMoneyToMinor(row.querySelector('[data-component-base]').value,currency),input:input,unit_amount_minor:wizardMoneyToMinor(row.querySelector('[data-component-amount]').value,currency)};
    else if(type==='lookup'){
      if(byKey[input].kind!=='select'&&byKey[input].kind!=='text')throw new Error('Lookup row '+key+' must reference a choice or text input.');
      amount={type:'lookup',input:input,prices:wizardParseLookup(row.querySelector('[data-component-details]').value,currency,key)};
    }else if(type==='graduated'||type==='volume')amount={type:type,input:input,tiers:wizardParseTiers(row.querySelector('[data-component-details]').value,currency,key)};
    else if(type==='package'){
      var packageSize=Number(row.querySelector('[data-component-package-size]').value);
      if(!Number.isSafeInteger(packageSize)||packageSize<1)throw new Error('Package size on '+key+' must be a positive whole number.');
      amount={type:'package',input:input,units_per_package:packageSize,package_amount_minor:wizardMoneyToMinor(row.querySelector('[data-component-amount]').value,currency),rounding:row.querySelector('[data-component-rounding]').value};
    }else throw new Error('Price row '+key+' uses an unknown calculation.');
    var conditionType=row.querySelector('[data-component-condition]').value,conditionInput=row.querySelector('[data-condition-input]').value.trim(),rawCondition=row.querySelector('[data-condition-value]').value.trim();var condition={op:'always'};
    if(conditionType==='advanced_preserved'){
      try{condition=JSON.parse(row.dataset.preservedCondition)}catch(_error){throw new Error('Advanced condition on '+key+' could not be preserved.')}
    }else if(conditionType!=='always'){
      if(!byKey[conditionInput])throw new Error('Condition on '+key+' must reference an existing input.');
      if(conditionType==='present')condition={op:'present',input:conditionInput};
      else if(conditionType==='in'){
        var conditionValues=rawCondition.split(',').map(function(value){return value.trim()}).filter(Boolean);
        if(conditionValues.length===0)throw new Error('Condition on '+key+' needs at least one comparison value.');
        condition={op:'in',input:conditionInput,values:conditionValues.map(function(value){return wizardConditionValue(value,byKey[conditionInput])})};
      }else{
        if(rawCondition==='')throw new Error('Condition on '+key+' needs a comparison value.');
        condition={op:conditionType,input:conditionInput,value:wizardConditionValue(rawCondition,byKey[conditionInput])};
      }
    }
    var quantity={type:'fixed',value:1},metadata={};
    if(row.dataset.preservedQuantity){try{quantity=JSON.parse(row.dataset.preservedQuantity)}catch(_error){throw new Error('Advanced quantity rule on '+key+' could not be preserved.')}}
    if(row.dataset.preservedMetadata){try{metadata=JSON.parse(row.dataset.preservedMetadata)}catch(_error){throw new Error('Metadata on '+key+' could not be preserved.')}}
    var component={key:key,label:label,description:row.querySelector('[data-component-description]').value.trim(),sort_order:index,required:row.querySelector('[data-component-required]').checked,amount:amount,quantity:quantity,condition:condition,metadata:metadata};
    if(subscription)component.recurrence={interval:interval,interval_count:intervalCount};
    components.push(component);
  });
  if(components.length===0)throw new Error('Add at least one itemized price row.');return components;
}
function buildProductWizardOffer(){
  var template=productWizardTemplate(),subscription=productWizardIsSubscription(),configurable=productWizardIsConfigurable();
  var currency=wizardById('wizard-currency').value.trim().toUpperCase();
  if(!/^[A-Z]{3}$/.test(currency))throw new Error('Currency must be a three-letter ISO code.');
  var interval=wizardById('wizard-interval').value,intervalCount=Number(wizardById('wizard-interval-count').value||1);
  if(subscription&&(!Number.isInteger(intervalCount)||intervalCount<1||intervalCount>36))throw new Error('Billing interval count must be between 1 and 36.');
  var variables=[],components=[];
  if(configurable){variables=collectWizardVariables();components=collectWizardComponents(variables,currency,subscription,interval,intervalCount)}
  else{
    var amount=wizardMoneyToMinor(wizardById('wizard-price').value,currency);
    var component={key:'price',label:wizardById('wizard-name').value.trim()||'Price',sort_order:0,required:true,amount:{type:'fixed',unit_amount_minor:amount},quantity:{type:'fixed',value:1},condition:{op:'always'}};
    if(subscription)component.recurrence={interval:interval,interval_count:intervalCount};components=[component];
  }
  var tiered=components.some(function(component){return component.amount.type==='graduated'||component.amount.type==='volume'});
  var taxBehavior=wizardById('wizard-tax-behavior').value,collectShipping=wizardById('wizard-shipping-address').checked;
  var shippingCountries=collectShipping?wizardParseShippingCountries(wizardById('wizard-shipping-countries').value):[];
  var shippingOptions=collectShipping?wizardParseShippingOptions(wizardById('wizard-shipping-options').value,currency,taxBehavior):[];
  var minimumRaw=wizardById('wizard-minimum-total').value.trim(),maximumRaw=wizardById('wizard-maximum-total').value.trim();
  var minimumTotal=minimumRaw?wizardMoneyToMinor(minimumRaw,currency):null,maximumTotal=maximumRaw?wizardMoneyToMinor(maximumRaw,currency):null;
  if(maximumTotal!==null&&maximumTotal<=0)throw new Error('Maximum item total must be greater than zero.');
  if(minimumTotal!==null&&maximumTotal!==null&&minimumTotal>maximumTotal)throw new Error('Minimum item total must not exceed maximum item total.');
  var checkout={allow_promotion_codes:wizardById('wizard-promotions').checked,automatic_tax:wizardById('wizard-automatic-tax').checked,collect_billing_address:wizardById('wizard-billing-address').checked,collect_shipping_address:collectShipping,allowed_shipping_countries:shippingCountries,shipping_options:shippingOptions,create_customer:wizardById('wizard-create-customer').checked,require_terms_consent:wizardById('wizard-terms').checked,trial_days:subscription?Number(wizardById('wizard-trial-days').value||0):0};
  if(minimumTotal!==null)checkout.minimum_total_minor=minimumTotal;if(maximumTotal!==null)checkout.maximum_total_minor=maximumTotal;
  return {name:wizardById('wizard-name').value.trim()||'New offer',mode:subscription?'subscription':'payment',currency:currency,pricing_model:configurable?'components':'fixed',recurring_interval:subscription?interval:null,interval_count:subscription?intervalCount:1,usage_type:'licensed',billing_scheme:tiered?'tiered':'per_unit',tax_behavior:taxBehavior,variables:variables,components:components,checkout:checkout};
}
function buildProductWizardPayload(){
  var name=wizardById('wizard-name').value.trim();if(!name)throw new Error('Product name is required.');
  var slug=wizardById('wizard-slug').value.trim()||productWizardSlug(name);if(!slug)throw new Error('Product name must contain at least one letter or number.');
  var offer=buildProductWizardOffer();var tags=wizardById('wizard-tags').value.split(',').map(function(v){return v.trim()}).filter(Boolean);
  var product={name:name,slug:slug,description:wizardById('wizard-description').value.trim(),image_url:wizardById('wizard-image').value.trim(),tags:tags,currency:offer.currency,fulfillment_kind:wizardById('wizard-fulfillment').value,product_template_id:productWizardTemplate(),metadata:{impresspress_template:productWizardTemplate()}};
  return {product:product,offer:offer};
}
function renderProductWizardReview(){
  var target=wizardById('wizard-review');target.replaceChildren();
  try{
    var built=buildProductWizardPayload(),offer=built.offer;
    var title=document.createElement('h4');title.textContent=built.product.name;target.appendChild(title);
    var summary=document.createElement('p');summary.className='text-muted text-sm';summary.textContent=(offer.mode==='subscription'?'Subscription':'One-time payment')+' · '+offer.currency+' · '+(offer.pricing_model==='fixed'?'Fixed price':'Configurable rows');target.appendChild(summary);
    var list=document.createElement('ul');
    offer.components.forEach(function(component){var item=document.createElement('li'),rule=component.amount,description='';
      if(rule.type==='fixed')description=wizardMinorToDisplay(rule.unit_amount_minor,offer.currency)+' '+offer.currency;
      else if(rule.type==='per_unit')description=wizardMinorToDisplay(rule.unit_amount_minor,offer.currency)+' '+offer.currency+' per '+rule.input;
      else if(rule.type==='flat_plus_per_unit')description=wizardMinorToDisplay(rule.base_amount_minor,offer.currency)+' + '+wizardMinorToDisplay(rule.unit_amount_minor,offer.currency)+' '+offer.currency+' per '+rule.input;
      else if(rule.type==='lookup')description=Object.keys(rule.prices).length+' lookup price(s) selected by '+rule.input;
      else if(rule.type==='graduated'||rule.type==='volume')description=rule.tiers.length+' '+rule.type+' tier(s) based on '+rule.input;
      else if(rule.type==='package')description=wizardMinorToDisplay(rule.package_amount_minor,offer.currency)+' '+offer.currency+' per '+rule.units_per_package+' '+rule.input;
      item.textContent=component.label+': '+description+(component.condition.op!=='always'?' when '+component.condition.input+' '+component.condition.op.replace(/_/g,' ')+' '+String(component.condition.value||component.condition.values||''):'');list.appendChild(item)});
    target.appendChild(list);
    var options=document.createElement('p');options.className='text-muted text-sm';options.textContent=offer.variables.length+' customer input(s), '+offer.components.length+' price row(s)'+(offer.checkout.minimum_total_minor!==undefined?', minimum '+wizardMinorToDisplay(offer.checkout.minimum_total_minor,offer.currency)+' '+offer.currency:'')+(offer.checkout.maximum_total_minor!==undefined?', maximum '+wizardMinorToDisplay(offer.checkout.maximum_total_minor,offer.currency)+' '+offer.currency:'')+(offer.checkout.automatic_tax?', automatic tax':'')+(offer.checkout.allow_promotion_codes?', promotion codes':'');target.appendChild(options);
  }catch(error){productWizardShowError(error.message)}
}
async function productWizardRequest(path,method,body){
  var response=await fetch(path,{method:method,credentials:'same-origin',headers:{'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});var data={};try{data=await response.json()}catch(_){}
  if(!response.ok)throw new Error(data.message||data.error||'The server rejected the product configuration.');return data;
}
async function submitProductWizard(intent){
  productWizardClearError();var buttons=[wizardById('wizard-save-draft'),wizardById('wizard-publish')];var productId='';
  try{
    var built=buildProductWizardPayload();buttons.forEach(function(button){button.disabled=true});
    var config=window.__productWizardConfig;var created=await productWizardRequest(config.product_collection,'POST',built.product);productId=created.id;
    if(!productId)throw new Error('Product creation returned no product ID.');
    var offerCollection=config.product_collection+'/'+encodeURIComponent(productId)+'/offers';var managed=await productWizardRequest(offerCollection,'POST',built.offer);var offerId=managed.offer&&managed.offer.id;
    if(!offerId)throw new Error('Pricing creation returned no offer ID.');
    if(intent==='publish'){
      await productWizardRequest(offerCollection+'/'+encodeURIComponent(offerId)+'/publish','POST',{});
      await productWizardRequest(config.product_collection+'/'+encodeURIComponent(productId),'PATCH',{status:'active'});
    }
    window.location.assign(config.return_url+'?created='+encodeURIComponent(productId)+(intent==='publish'?'&published=1':''));
  }catch(error){
    productWizardShowError((productId?'Product draft '+productId+' was created, but setup did not finish. ':'')+(error.message||'Product setup failed.'));
    buttons.forEach(function(button){button.disabled=false});
  }
}
function initProductWizard(){productWizardTemplateChanged();productWizardShippingChanged();productWizardShowStep(1,false)}
// The wizard's controls, delegated. They used to be `onclick`/`onchange`
// attributes; see the rule in ui/assets/chrome.js. The verbs are `pw-`
// prefixed because `data-action` is one namespace shared by every script on
// the page -- the product manager loads this file too, for the visual editor's
// "+ Add input"/"+ Add row" buttons, which is why those two verbs work on both
// pages from this one listener.
//
// Guarded for the reason spelled out at the top of products-seller-admin.js: an htmx tab swap
// re-executes this script against the same `document`, so an unguarded
// registration accumulates one listener per visit.
(function(){
  if(window.__productWizardDelegated)return;
  window.__productWizardDelegated=true;
  document.addEventListener('submit',function(e){
    if(e.target&&e.target.id==='product-wizard-form')e.preventDefault();
  });
  document.addEventListener('click',function(e){
    if(!(e.target instanceof Element))return;
    var el=e.target.closest('[data-action]');
    if(!el)return;
    var action=el.getAttribute('data-action');
    if(action==='pw-add-variable')addWizardVariable();
    else if(action==='pw-add-component')addWizardComponent();
    else if(action==='pw-previous')productWizardPrevious();
    else if(action==='pw-next')productWizardNext();
    else if(action==='pw-submit')submitProductWizard(el.getAttribute('data-wizard-intent'));
  });
  document.addEventListener('change',function(e){
    var el=e.target;
    if(!(el instanceof Element))return;
    var action=el.getAttribute('data-action');
    if(action==='pw-template-changed')productWizardTemplateChanged();
    else if(action==='pw-shipping-changed')productWizardShippingChanged();
  });
})();
// This file is loaded by two pages. The wizard page bootstraps
// `window.__productWizardConfig` and wants the wizard initialised; the product
// manager loads the same file only for the helper functions its visual offer
// editor reuses (`addWizardVariable`, `addWizardComponent`,
// `collectWizardComponents`, `wizardMinorToDisplay`) and has no wizard DOM to
// initialise. The init call used to sit in the page's inline `<script>` after
// the source; now that the source is an external file it has to live here,
// because htmx inserts a swapped-in `<script src>` with `async=false` (so it
// executes in order against other external scripts) while a following INLINE
// script executes immediately on insertion -- a trailing inline
// `initProductWizard()` would therefore run before this file had loaded.
// Latent, and not reachable today: nothing ever clears
// `window.__productWizardConfig`, so if the products links ever became
// htmx-boosted, loading the wizard and then swapping to the manager would
// leave a stale truthy global and init the wizard against the manager's DOM.
// Today both pages are reached by plain links, so each gets a fresh document.
if(window.__productWizardConfig)initProductWizard();
