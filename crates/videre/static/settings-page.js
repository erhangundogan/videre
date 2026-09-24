// The /settings page: export, import and reset of this library's gallery
// settings through /api/settings. Export carries only `routes`, the portable
// preferences; where a library resumes stays with that library.
(function(){
  var status=document.getElementById('settings-status');
  function report(r,done){
    var msg=done;
    if(r.ignored&&r.ignored.length)msg+=' Ignored, wrong type: '+r.ignored.join(', ')+'.';
    status.textContent=msg;
  }
  function send(method,body,done){
    return fetch('/api/settings',{method:method,
      headers:{'Content-Type':'application/json'},body:JSON.stringify(body)})
      .then(function(r){
        if(r.status===409)throw new Error('the settings file is unreadable; fix or delete it first');
        if(!r.ok)throw new Error('the server answered '+r.status);
        return r.json();
      })
      .then(function(r){ VIDERE_SETTINGS=r.effective; report(r,done); })
      .catch(function(e){ status.textContent='Not saved: '+e.message+'.'; });
  }
  document.getElementById('settings-export').addEventListener('click',function(){
    fetch('/api/settings').then(function(r){ return r.json(); }).then(function(r){
      var out={routes:(r.overrides&&r.overrides.routes)||{}};
      var a=document.createElement('a');
      a.href=URL.createObjectURL(new Blob([JSON.stringify(out,null,2)+'\n'],{type:'application/json'}));
      a.download='videre-gallery-settings.json';
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(function(){ URL.revokeObjectURL(a.href); },0);
      status.textContent='Exported.';
    }).catch(function(){ status.textContent='Not exported: the server could not be reached.'; });
  });
  document.getElementById('settings-import').addEventListener('change',function(e){
    var input=e.target,file=input.files[0];
    if(!file)return;
    file.text().then(function(text){
      var doc;
      try{ doc=JSON.parse(text); }
      catch(err){ status.textContent='Not imported: the file is not valid JSON.'; return; }
      if(!doc||typeof doc!=='object'||!doc.routes||typeof doc.routes!=='object'||Array.isArray(doc.routes)){
        status.textContent='Not imported: the file has no "routes" object.';
        return;
      }
      return send('PUT',{routes:doc.routes},'Imported.');
    }).finally(function(){ input.value=''; });
  });
  document.getElementById('settings-reset').addEventListener('click',function(){
    if(!confirm('Reset every gallery setting for this library to its default?'))return;
    send('PUT',{routes:{}},'Reset to defaults.');
  });
})();
