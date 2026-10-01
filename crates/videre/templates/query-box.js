// The nav's query box: the query string, shown as chips.
//
// The string stays the source of truth: chips are a view of its keyed terms,
// and whatever they cannot show exactly stays one text chip with its raw text.
// Free words stay in the input, where they are typed, because they rank
// rather than filter. Submitting sends chips and words as one `q`.
//
// - A chip is one key: one value, or several joined as any (OR) or all
//   (AND), optionally negated. `-tag:a -tag:b` is "none of a, b", so it
//   reads back as one negated any chip.
// - Every term joined by a top-level OR forms one alternative (the query
//   language's rule), so those terms become one chip: any, when they are
//   values of one key, else a text chip.
// - A chip's not, any/all and remove apply at once; accepting a suggestion
//   only composes, and Enter applies.
// - Suggestions come from /api/query/suggest, the function the shell
//   completer uses too, for the term being typed in the input.
(function(){
  var form=document.querySelector('form.secnav-search');
  var input=document.getElementById('nav-search');
  var box=document.getElementById('qbox');
  var list=document.getElementById('qbox-suggest');
  if(!form||!input||!box||!list)return;
  var live=location.protocol==='http:'||location.protocol==='https:';
  // Keys that hold several values per file, where a second value most often
  // means "both"; for the rest two values can only mean "either".
  var MULTI={person:1,tag:1};
  var PLACEHOLDER=input.placeholder;
  // The pages that narrow by a query; the rest ignore one, so the box sends
  // a query from them to the Library, and the nav carries it only here.
  var HONOURS=/^\/((date|map|events)(\/.*)?)?$/;
  window.videreQueryHonours=function(path){return HONOURS.test(path);};
  var labels={};
  var chips=[];

  // ---- the string, as terms ------------------------------------------------

  function lex(s){
    var out=[],i=0;
    while(i<s.length){
      var c=s[i];
      if(/\s/.test(c)){i++;continue;}
      if(c==='('||c===')'){out.push({t:c,start:i,end:i+1});i++;continue;}
      var start=i,quoted=false;
      while(i<s.length){
        c=s[i];
        if(c==='"')quoted=!quoted;
        else if(!quoted&&(/\s/.test(c)||c==='('||c===')'))break;
        i++;
      }
      var text=s.slice(start,i);
      if(text==='OR'||text==='AND'||text==='NOT'||text==='-')out.push({t:text==='-'?'NOT':text,start:start,end:i});
      else out.push({t:'term',text:text,start:start,end:i});
    }
    return out;
  }

  // `key:value`, or null for a word. The value is unquoted.
  function keyed(text){
    var m=/^([A-Za-z]+):(.*)$/.exec(text);
    if(!m)return null;
    var v=m[2];
    if(v.length>1&&v[0]==='"'&&v[v.length-1]==='"')v=v.slice(1,-1);
    return {key:m[1].toLowerCase(),value:v};
  }

  // Top-level items: terms and groups, with negation and OR joins.
  function items(s){
    var toks=lex(s),out=[],neg=false,negStart=-1,joinNext=false;
    for(var i=0;i<toks.length;i++){
      var tk=toks[i];
      if(tk.t==='NOT'){neg=true;negStart=tk.start;continue;}
      if(tk.t==='AND')continue;
      if(tk.t==='OR'){ if(out.length)out[out.length-1].orNext=true; continue; }
      var item;
      if(tk.t==='('){
        var depth=1,j=i+1;
        while(j<toks.length&&depth){ if(toks[j].t==='(')depth++; else if(toks[j].t===')')depth--; j++; }
        var end=toks[j-1].end;
        item={kind:'group',inner:s.slice(tk.start+1,depth?end:end-1),start:tk.start,end:end};
        i=j-1;
      }else if(tk.t===')'){
        continue;
      }else{
        var text=tk.text,n=false;
        if(text[0]==='-'&&text.length>1){n=true;text=text.slice(1);}
        var kv=keyed(text);
        item=kv?{kind:'kv',key:kv.key,value:kv.value,start:tk.start,end:tk.end,neg:n}
               :{kind:'word',start:tk.start,end:tk.end};
      }
      if(neg){item.neg=!item.neg;item.start=negStart;}
      neg=false;
      item.raw=s.slice(item.start,item.end);
      item.orPrev=out.length>0&&!!out[out.length-1].orNext;
      out.push(item);
    }
    return out;
  }

  // A group chips can show: one key's values, all OR-joined or all ANDed.
  function groupChip(item){
    var inner=items(item.inner);
    if(!inner.length)return null;
    var key=inner[0].key,ors=0;
    for(var i=0;i<inner.length;i++){
      var it=inner[i];
      if(it.kind!=='kv'||it.neg||it.key!==key)return null;
      if(i>0&&it.orPrev)ors++;
    }
    if(ors&&ors!==inner.length-1)return null;
    return {key:key,values:inner.map(function(x){return x.value;}),mode:ors?'any':'all',neg:!!item.neg};
  }

  function parse(s){
    var all=items(s),out=[],words=[],alternative=[];
    all.forEach(function(it,i){
      if(it.orPrev||it.orNext){alternative.push(it);return;}
      if(it.kind==='word'){words.push(it.raw);return;}
      if(it.kind==='group'){ out.push(groupChip(it)||{text:it.raw}); return; }
      // Same key, same sign: positive values all match, negated ones none do.
      var mode=it.neg?'any':'all';
      var into=out.find(function(c){return !c.text&&c.key===it.key&&c.neg===it.neg&&c.merged;});
      if(into){ if(into.values.indexOf(it.value)<0){into.values.push(it.value);into.mode=mode;} return; }
      out.push({key:it.key,values:[it.value],mode:mode,neg:it.neg,merged:true});
    });
    if(alternative.length){
      var key=alternative[0].key;
      var simple=alternative.every(function(it){return it.kind==='kv'&&!it.neg&&it.key===key;});
      out.push(simple?{key:key,values:alternative.map(function(it){return it.value;}),mode:'any',neg:false}
                     :{text:alternative.map(function(it){return it.raw;}).join(' OR ')});
    }
    out.forEach(function(c){delete c.merged;});
    return {chips:out,words:words.join(' ')};
  }

  // ---- chips, as the string ----------------------------------------------

  // As `query_lang::quoted`: quoted when it has a space or query syntax.
  function quote(v){
    var plain=v!==''&&v[0]!=='-'&&['OR','AND','NOT','TO'].indexOf(v)<0&&!/[\s():"'\[\]{}^~*\\]/.test(v);
    return plain?v:'"'+v+'"';
  }

  function serializeChip(c){
    if(c.text)return c.text;
    var terms=c.values.map(function(v){return c.key+':'+quote(v);});
    var body=terms.length===1?terms[0]
      :c.mode==='any'?'('+terms.join(' OR ')+')'
      :(c.neg?'('+terms.join(' ')+')':terms.join(' '));
    return (c.neg?'-':'')+body;
  }

  function query(){
    var parts=chips.map(serializeChip);
    var words=input.value.trim();
    if(words)parts.push(words);
    return parts.join(' ');
  }

  // Other parameters of the page (a date range) stay as they are. Words rank,
  // which only the Library does, so a query with words goes there.
  function apply(){
    var q=query();
    var here=HONOURS.test(location.pathname)&&(location.pathname==='/'||!input.value.trim());
    var params=here?new URLSearchParams(location.search):new URLSearchParams();
    if(q)params.set('q',q); else params.delete('q');
    var rest=params.toString().replace(/\+/g,'%20');
    location.href=(here?location.pathname:'/')+(rest?'?'+rest:'');
  }

  // ---- rendering -------------------------------------------------------------

  function el(tag,cls,text){
    var e=document.createElement(tag);
    if(cls)e.className=cls;
    if(text!=null)e.textContent=text;
    return e;
  }

  function button(cls,text,label,onclick){
    var b=el('button',cls,text);
    b.type='button';
    if(label)b.setAttribute('aria-label',label);
    b.addEventListener('click',function(e){e.preventDefault();e.stopPropagation();onclick();});
    return b;
  }

  function shown(c,v){
    return c.key==='person'&&labels['person:'+v]?labels['person:'+v]:v;
  }

  function render(){
    box.querySelectorAll('.qchip').forEach(function(n){n.remove();});
    chips.forEach(function(c,i){
      var chip=el('span','qchip'+(c.text?' qchip-text':'')+(c.neg?' qchip-neg':''));
      if(c.text){
        chip.appendChild(el('span','qchip-body',c.text));
      }else{
        // The toggle is a mark, so only a negated chip reads "not".
        chip.appendChild(button('qchip-not','\u00ac',c.neg?'Include again':'Exclude',function(){c.neg=!c.neg;apply();}));
        var body=el('span','qchip-body');
        if(c.neg)body.appendChild(el('span','qchip-neg-word','not'));
        body.appendChild(el('span','qchip-key',c.key));
        c.values.forEach(function(v,j){
          if(j>0)body.appendChild(el('span','qchip-join',c.mode==='any'?'or':'and'));
          body.appendChild(el('span','qchip-val',shown(c,v)));
        });
        chip.appendChild(body);
        if(c.values.length>1){
          chip.appendChild(button('qchip-mode',c.mode,'Match '+(c.mode==='any'?'all':'any')+' of these',function(){
            c.mode=c.mode==='any'?'all':'any';apply();
          }));
        }
      }
      chip.appendChild(button('qchip-x','×','Remove',function(){chips.splice(i,1);apply();}));
      chip.querySelector('.qchip-body').addEventListener('click',function(){edit(i);});
      box.insertBefore(chip,input);
    });
    input.placeholder=chips.length?'':PLACEHOLDER;
  }

  // Take chip i back into the input as text.
  function edit(i){
    var text=serializeChip(chips[i]);
    chips.splice(i,1);
    var rest=input.value.trim();
    input.value=rest?text+' '+rest:text;
    render();
    input.focus();
    input.setSelectionRange(text.length,text.length);
  }

  // ---- suggestions -----------------------------------------------------------

  var options=[],active=-1,start=0,seq=0,timer=null;

  function close(){
    list.hidden=true;
    list.innerHTML='';
    options=[];active=-1;
    input.setAttribute('aria-expanded','false');
  }

  function highlight(i){
    active=i;
    Array.prototype.forEach.call(list.children,function(li,j){
      li.setAttribute('aria-selected',String(j===i));
    });
  }

  function show(data){
    list.innerHTML='';
    start=data.start;
    // Chips stand in for OR and grouping, so operators are not offered here.
    options=(data.items||[]).filter(function(it){return it.kind!=='operator';});
    if(!options.length){close();return;}
    options.forEach(function(it,i){
      var li=el('li','qbox-opt');
      li.setAttribute('role','option');
      if(it.face_id!=null){
        var img=el('img','qbox-face');
        img.src='/api/faces/'+it.face_id+'/image';
        img.alt='';
        li.appendChild(img);
      }
      li.appendChild(el('span','qbox-opt-label',it.kind==='key'?it.insert:it.label));
      if(it.kind==='value'&&it.label!==it.insert.slice(it.insert.indexOf(':')+1)){
        li.appendChild(el('span','qbox-opt-value',it.insert));
      }
      if(it.count!=null)li.appendChild(el('span','qbox-opt-count',it.count.toLocaleString()));
      li.addEventListener('mousedown',function(e){e.preventDefault();accept(i);});
      list.appendChild(li);
    });
    list.hidden=false;
    input.setAttribute('aria-expanded','true');
    highlight(0);
  }

  function fetchSuggestions(){
    if(!live)return;
    var mine=++seq,cursor=input.selectionStart==null?input.value.length:input.selectionStart;
    fetch('/api/query/suggest?q='+encodeURIComponent(input.value)+'&cursor='+cursor+'&limit=12')
      .then(function(r){return r.ok?r.json():null;})
      .then(function(d){ if(mine===seq&&d&&document.activeElement===input)show(d); else if(mine===seq)close(); })
      .catch(close);
  }

  function schedule(){
    clearTimeout(timer);
    timer=setTimeout(fetchSuggestions,120);
  }

  function accept(i){
    var it=options[i],v=input.value;
    var cursor=input.selectionStart==null?v.length:input.selectionStart;
    if(it.kind==='key'){
      input.value=v.slice(0,start)+it.insert+v.slice(cursor);
      var at=start+it.insert.length;
      input.setSelectionRange(at,at);
      fetchSuggestions();
      return;
    }
    // A value becomes a chip, or joins its key's chip.
    var neg=start>0&&v[start-1]==='-';
    input.value=(v.slice(0,neg?start-1:start)+v.slice(cursor)).replace(/\s+/g,' ').trim();
    var kv=keyed(it.insert);
    if(it.label!==kv.value)labels[kv.key+':'+kv.value]=it.label;
    var into=chips.find(function(c){return !c.text&&c.key===kv.key&&c.neg===neg;});
    if(into){
      if(into.values.indexOf(kv.value)<0){
        into.values.push(kv.value);
        if(into.values.length===2)into.mode=MULTI[kv.key]&&!neg?'all':'any';
      }
    }else{
      chips.push({key:kv.key,values:[kv.value],mode:'all',neg:neg});
    }
    close();
    render();
    input.focus();
  }

  input.addEventListener('input',schedule);
  input.addEventListener('focus',function(){ if(input.value)schedule(); });
  input.addEventListener('blur',function(){ setTimeout(close,100); });
  input.addEventListener('keydown',function(e){
    var open=!list.hidden&&options.length;
    if(e.key==='ArrowDown'&&open){e.preventDefault();highlight((active+1)%options.length);}
    else if(e.key==='ArrowUp'&&open){e.preventDefault();highlight((active-1+options.length)%options.length);}
    else if((e.key==='Enter'||e.key==='Tab')&&open&&active>=0){e.preventDefault();accept(active);}
    else if(e.key==='Escape'&&open){e.preventDefault();close();}
    else if(e.key==='Backspace'&&chips.length&&input.selectionStart===0&&input.selectionEnd===0){
      e.preventDefault();
      edit(chips.length-1);
    }
  });
  box.addEventListener('click',function(e){ if(e.target===box)input.focus(); });
  form.addEventListener('submit',function(e){ e.preventDefault(); close(); apply(); });

  var q=new URLSearchParams(location.search).get('q');
  if(q){
    var parsed=parse(q);
    chips=parsed.chips;
    input.value=parsed.words;
    document.querySelectorAll('nav.secnav a[href]').forEach(function(a){
      var href=a.getAttribute('href');
      if(href.indexOf('?')<0&&HONOURS.test(href))a.setAttribute('href',href+'?q='+encodeURIComponent(q));
    });
  }
  input.setAttribute('role','combobox');
  input.setAttribute('aria-controls','qbox-suggest');
  input.setAttribute('aria-expanded','false');
  input.setAttribute('aria-autocomplete','list');
  render();
})();
