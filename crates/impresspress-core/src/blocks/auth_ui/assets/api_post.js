async function apiPost(path,body){
  var r;
  try{r=await fetch(path,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)})}
  catch(ex){throw new Error('The request did not reach the app'+(ex&&ex.message?' ('+ex.message+')':'')+'. Check your connection and try again.')}
  var d;
  try{d=await r.json()}
  catch(ex){throw new Error('The app answered HTTP '+r.status+' with no message. Reload the page and try again.')}
  if(!r.ok||!d||d.error){
    var m=d&&((typeof d.message==='string'&&d.message)||(typeof d.error==='string'&&d.error));
    throw new Error(m||('The app answered HTTP '+r.status+'.'));
  }
  return d;
}
