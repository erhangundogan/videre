// The settings pages' form: /settings/config edits `videre config` keys,
// /settings/gallery the gallery settings. Both draw rows from a schema (the
// key table in library_config, gallery-schema.json), check each value as it
// is typed, and save every change at once from the bar at the bottom. A row
// whose value differs from its default is tinted and gets a reset button.
(function(){
  var root=document.getElementById('settings-form');
  if(!root)return;
  var mode=root.dataset.kind;

  function same(a,b){ return JSON.stringify(a)===JSON.stringify(b); }
  function el(tag,cls,text){
    var e=document.createElement(tag);
    if(cls)e.className=cls;
    if(text!==undefined&&text!==null)e.textContent=text;
    return e;
  }
  function shown(v,spec){
    if(v===null||v===undefined)return 'unset';
    if(spec.type==='bool')return v?'on':'off';
    if(Array.isArray(v))return v.join(', ');
    return String(v)+(spec.unit?' '+spec.unit:'');
  }
  // The same rules and messages as the server's checks
  // (settings::validate, config_form::parse).
  function refusal(spec,v,optional){
    if(v===null)return optional?null:'Enter a value';
    var range=spec.max===undefined||spec.max===null
      ?(spec.min===undefined?'':' of at least '+spec.min)
      :' from '+spec.min+' to '+spec.max;
    var inRange=function(n){
      return (spec.min===undefined||spec.min===null||n>=spec.min)&&(spec.max===undefined||spec.max===null||n<=spec.max);
    };
    switch(spec.type){
      case 'bool': return typeof v==='boolean'?null:'Choose on or off';
      case 'int': return (typeof v==='number'&&Number.isInteger(v)&&inRange(v))?null:'Enter a whole number'+range;
      case 'number': return (typeof v==='number'&&isFinite(v)&&inRange(v))?null:'Enter a number'+range;
      case 'enum': return spec.options.indexOf(v)>=0?null:'Choose one of '+spec.options.join(', ');
      case 'set': return (Array.isArray(v)&&v.every(function(x){return spec.options.indexOf(x)>=0;}))?null:'Choose from '+spec.options.join(', ');
      case 'model': return (typeof v==='string'&&/^[^/]+\/[^/]+$/.test(v))?null:'Enter a model as owner/name';
      default: return null;
    }
  }

  // Each row: {id, label, key, hint, spec, value, def, optional, group}.
  var rows=[],pending={},errors={},serverErrors={};

  function current(r){ return Object.prototype.hasOwnProperty.call(pending,r.id)?pending[r.id]:r.value; }
  // A pending reset (null) shows the default.
  function displayed(r){ var v=current(r); return v===null?r.def:v; }
  function overridden(r){
    var v=current(r);
    if(v===null)return false;
    return !same(v,r.def);
  }

  function control(r,onChange){
    var spec=r.spec,v=displayed(r),wrap;
    if(spec.type==='bool'){
      wrap=el('label','set-switch');
      var box=el('input');box.type='checkbox';box.checked=v===true;
      box.setAttribute('aria-label',r.label);
      box.addEventListener('change',function(){ onChange(box.checked); });
      wrap.appendChild(box);wrap.appendChild(el('span','set-switch-track'));
      return wrap;
    }
    if(spec.type==='enum'){
      var sel=el('select');sel.setAttribute('aria-label',r.label);
      spec.options.forEach(function(o){ var op=el('option',null,o);op.value=o;sel.appendChild(op); });
      sel.value=v;
      sel.addEventListener('change',function(){ onChange(sel.value); });
      return sel;
    }
    if(spec.type==='set'){
      wrap=el('div','set-checks');
      spec.options.forEach(function(o){
        var l=el('label'),c=el('input');c.type='checkbox';c.value=o;c.checked=(v||[]).indexOf(o)>=0;
        c.addEventListener('change',function(){
          onChange(spec.options.filter(function(x){ return wrap.querySelector('input[value="'+x+'"]').checked; }));
        });
        l.appendChild(c);l.appendChild(document.createTextNode(' '+o));wrap.appendChild(l);
      });
      return wrap;
    }
    if(spec.type==='int'||spec.type==='number'){
      var input=el('input');input.type=spec.control==='slider'?'range':'number';
      if(spec.min!==undefined&&spec.min!==null)input.min=spec.min;
      if(spec.max!==undefined&&spec.max!==null)input.max=spec.max;
      input.step=spec.step||(spec.type==='int'?1:'any');
      input.value=v===null||v===undefined?'':v;
      input.placeholder=r.optional?'unset':'';
      input.setAttribute('aria-label',r.label);
      var read=function(){
        if(input.value.trim()==='')return r.optional?null:NaN;
        return Number(input.value);
      };
      if(input.type==='range'){
        wrap=el('span','set-slider');
        var out=el('output',null,input.value);
        input.addEventListener('input',function(){ out.textContent=input.value; onChange(read(),true); });
        wrap.appendChild(input);wrap.appendChild(out);
        return wrap;
      }
      input.addEventListener('input',function(){ onChange(read(),true); });
      return input;
    }
    var text=el('input');text.type='text';text.value=v||'';text.setAttribute('aria-label',r.label);
    text.addEventListener('input',function(){ onChange(text.value,true); });
    return text;
  }

  function setValue(r,v,typing){
    if(same(v,r.value))delete pending[r.id];else pending[r.id]=v;
    delete serverErrors[r.id];
    var problem=refusal(r.spec,v,r.optional);
    if(problem)errors[r.id]=problem;else delete errors[r.id];
    // While typing, keep focus in the field: refresh the row's state only.
    if(typing)paintRow(r);else render();
  }

  var rowEls={};
  function paintRow(r){
    var e=rowEls[r.id];if(!e)return;
    e.row.classList.toggle('ov',overridden(r));
    e.reset.hidden=!overridden(r);
    e.reset.title='Reset to default ('+shown(r.def,r.spec)+')';
    e.def.textContent=overridden(r)?'Default: '+shown(r.def,r.spec):'';
    var msg=errors[r.id]||serverErrors[r.id]||'';
    e.err.textContent=msg;
    e.row.classList.toggle('bad',!!msg);
    paintBar();
  }

  function rowEl(r){
    var row=el('div','set-row');row.dataset.key=r.id;
    var name=el('div','set-name');
    var title=el('div','set-label',r.label);
    title.appendChild(el('span','set-key',r.key));
    name.appendChild(title);
    if(r.hint)name.appendChild(el('div','set-hint',r.hint));
    var def=el('div','set-default');name.appendChild(def);
    var err=el('div','set-error');err.setAttribute('role','alert');name.appendChild(err);
    row.appendChild(name);
    var reset=el('button','reset-button');reset.type='button';
    reset.setAttribute('aria-label','Reset '+r.label+' to default');
    reset.innerHTML='<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/></svg>';
    reset.addEventListener('click',function(){
      // null removes the key, so the default applies again.
      if(same(r.value,r.def))delete pending[r.id];else pending[r.id]=null;
      delete errors[r.id];delete serverErrors[r.id];
      render();
    });
    row.appendChild(reset);
    row.appendChild(control(r,function(v,typing){ setValue(r,v,typing); }));
    rowEls[r.id]={row:row,reset:reset,def:def,err:err};
    paintRow(r);
    return row;
  }

  var bar,status,saveBtn,discardBtn,form;
  function paintBar(){
    if(!bar)return;
    var n=Object.keys(pending).length,bad=Object.keys(errors).length;
    saveBtn.disabled=n===0;discardBtn.disabled=n===0;
    if(bad)status.textContent=bad+(bad===1?' value needs':' values need')+' fixing before saving';
    else if(n&&!status.dataset.sticky)status.textContent=n+(n===1?' unsaved change':' unsaved changes');
    else if(!n&&!status.dataset.sticky)status.textContent='';
    status.classList.toggle('bad',bad>0);
  }

  // Groups: [{title, rows, sub: [{title, rows}]}], drawn as collapsible
  // panels with the count of overridden values in the heading.
  var groups=[];
  function render(){
    form.innerHTML='';rowEls={};
    var open=JSON.parse(form.dataset.open||'{}');
    groups.forEach(function(g){
      var all=g.rows.concat.apply(g.rows,g.sub.map(function(s){return s.rows;}));
      var details=el('details','set-group');details.open=open[g.title]!==false;
      details.addEventListener('toggle',function(){ open[g.title]=details.open;form.dataset.open=JSON.stringify(open); });
      var sum=el('summary',null,g.title);
      var count=all.filter(overridden).length;
      if(count)sum.appendChild(el('span','set-count',count+' overridden'));
      details.appendChild(sum);
      g.rows.forEach(function(r){ details.appendChild(rowEl(r)); });
      g.sub.forEach(function(s){
        var sd=el('details','set-sub');var key=g.title+'/'+s.title;sd.open=open[key]!==false;
        sd.addEventListener('toggle',function(){ open[key]=sd.open;form.dataset.open=JSON.stringify(open); });
        var ss=el('summary',null,s.title);
        var c=s.rows.filter(overridden).length;
        if(c)ss.appendChild(el('span','set-count',c+' overridden'));
        sd.appendChild(ss);
        s.rows.forEach(function(r){ sd.appendChild(rowEl(r)); });
        details.appendChild(sd);
      });
      form.appendChild(details);
    });
    paintBar();
  }

  function patchOf(){
    if(mode==='config')return pending;
    var patch={};
    Object.keys(pending).forEach(function(path){
      var parts=path.split('.'),doc=patch;
      for(var i=0;i<parts.length-1;i++){ doc=doc[parts[i]]=doc[parts[i]]||{}; }
      doc[parts[parts.length-1]]=pending[path];
    });
    return patch;
  }

  function say(text,bad){
    status.textContent=text;status.dataset.sticky='1';
    status.classList.toggle('bad',!!bad);
    setTimeout(function(){ delete status.dataset.sticky; },4000);
  }

  function save(){
    if(Object.keys(errors).length){ paintBar(); return; }
    var url=mode==='config'?'/api/config':'/api/settings';
    var type=mode==='config'?'application/json':'application/merge-patch+json';
    saveBtn.disabled=true;
    fetch(url,{method:'PATCH',headers:{'Content-Type':type},body:JSON.stringify(patchOf())})
      .then(function(res){ return res.json().catch(function(){ return {}; }).then(function(b){ return {res:res,body:b}; }); })
      .then(function(o){
        if(o.res.status===422&&o.body.errors){
          serverErrors=o.body.errors;
          rows.forEach(paintRow);
          say('Not saved: '+Object.keys(serverErrors).length+' value(s) refused',true);
          return;
        }
        if(!o.res.ok){
          var why=o.body.error||(o.res.status===409?'the settings file is unreadable; fix or delete it first':'the server answered '+o.res.status);
          say('Not saved: '+why,true);paintBar();return;
        }
        pending={};serverErrors={};
        var note='Saved';
        if(mode==='config'){
          rows.forEach(function(r){ r.value=o.body.values[r.id]; });
          var freed=o.body.deleted_street_map_bytes;
          if(typeof freed==='number')note+='. Deleted the street map, '+Math.round(freed/1e6)+' MB';
        }else{
          VIDERE_SETTINGS=o.body.effective;
          rows.forEach(function(r){ r.value=settingAt(VIDERE_SETTINGS,r.id); });
        }
        render();say(note);
      })
      .catch(function(){ say('Not saved: the server could not be reached',true); paintBar(); });
  }

  function build(){
    form=el('div','set-form');root.appendChild(form);
    bar=el('div','set-savebar');
    status=el('span','set-status');status.setAttribute('role','status');status.id='settings-form-status';
    discardBtn=el('button',null,'Discard');discardBtn.type='button';
    saveBtn=el('button','primary','Save');saveBtn.type='button';
    discardBtn.addEventListener('click',function(){ pending={};errors={};serverErrors={};render(); });
    saveBtn.addEventListener('click',save);
    bar.appendChild(status);bar.appendChild(discardBtn);bar.appendChild(saveBtn);
    root.appendChild(bar);
    window.addEventListener('beforeunload',function(e){ if(Object.keys(pending).length){ e.preventDefault(); e.returnValue=''; } });
  }

  function cap(s){ return s.charAt(0).toUpperCase()+s.slice(1); }

  if(mode==='config'){
    var cfg=VIDERE_CONFIG;
    var GROUP={'gallery-starts-watch':'Watch','export-xmp-on-watch':'Watch','search-min-match':'Search',
      'similar-min-score':'Search','street-detail':'Map'};
    var APPLIES={'next-run':'Applies from the next run','next-start':'Applies when the gallery or watch next starts'};
    var order=['General','Watch','Search','Logs','Map'],byName={};
    order.forEach(function(t){ byName[t]={title:t,rows:[],sub:[]}; });
    cfg.keys.forEach(function(k){
      var spec=k.kind;
      var g=GROUP[k.cli]||(k.cli.indexOf('watch-')===0?'Watch':k.cli.indexOf('log-')===0?'Logs':'General');
      var r={id:k.cli,label:k.label,key:k.cli,hint:k.hint+(APPLIES[k.applies]?'. '+APPLIES[k.applies]:''),
        spec:spec,value:k.value,def:k.default,optional:k.optional};
      rows.push(r);byName[g].rows.push(r);
    });
    groups=order.map(function(t){ return byName[t]; }).filter(function(g){ return g.rows.length; });
    build();
    if(cfg.error)say('The config could not be read, so these are the defaults: '+cfg.error,true);
    render();
    return;
  }

  // Gallery: grouped by the page a setting belongs to, then by its JSON
  // object, with the route's own object (routes.files, faces) at the top.
  var SECTIONS=[['library','Library'],['date','Date'],['search','Search'],['events','Events'],
    ['people','People'],['duplicates','Duplicates'],['map','Map']];
  var bySection={};
  SECTIONS.forEach(function(s){ bySection[s[0]]={title:s[1],rows:[],sub:[]}; });
  Object.keys(VIDERE_SETTINGS_SCHEMA).forEach(function(path){
    var spec=VIDERE_SETTINGS_SCHEMA[path];
    if(spec.hidden||!bySection[spec.section])return;
    var parts=path.split('.');
    var start=parts[0]==='routes'?2:1;
    var inner=parts.slice(start,parts.length-1).join('.');
    var r={id:path,label:spec.label,key:parts.slice(start).join('.'),hint:spec.hint,spec:spec,
      value:settingAt(VIDERE_SETTINGS,path),def:settingAt(VIDERE_SETTINGS_DEFAULTS,path),optional:false};
    rows.push(r);
    var sec=bySection[spec.section];
    if(!inner){ sec.rows.push(r); return; }
    var title=cap(inner.replace(/\./g,' '));
    var sub=sec.sub.filter(function(s){ return s.title===title; })[0];
    if(!sub){ sub={title:title,rows:[]};sec.sub.push(sub); }
    sub.rows.push(r);
  });
  groups=SECTIONS.map(function(s){ return bySection[s[0]]; }).filter(function(g){ return g.rows.length||g.sub.length; });
  build();
  render();
})();
