function apiError(message,status,refused){var e=new Error(message);e.status=status;e.refused=refused;return e}
async function apiPost(path,body){
  var r;
  try{r=await fetch(path,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)})}
  catch(ex){throw apiError('The request did not reach the app'+(ex&&ex.message?' ('+ex.message+')':'')+'. Check your connection and try again.',0,false)}
  var d;
  try{d=await r.json()}
  catch(ex){throw apiError('The app answered HTTP '+r.status+' with no message. Reload the page and try again.',r.status,false)}
  if(!r.ok||!d||d.error){
    var m=d&&((typeof d.message==='string'&&d.message)||(typeof d.error==='string'&&d.error));
    throw apiError(m||('The app answered HTTP '+r.status+'.'),r.status,true);
  }
  return d;
}
function keepSession(d){
  if(!d||!d.access_token)return;
  var secure=location.protocol==='https:'?'; Secure':'';
  document.cookie='auth_token='+d.access_token+'; Path=/; SameSite=Lax; Max-Age='+(d.expires_in||1800)+secure;
}
function hasKeptSession(){return /(?:^|;\s*)auth_token=[^;]/.test(document.cookie)}
