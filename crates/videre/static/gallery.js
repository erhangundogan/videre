
var PAGE=100,sorted=GROUPS.slice(),shown=0;

function escA(s){
  return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
}
function escH(s){
  return s?String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;'):'';
}
// Must agree with videre_core::disk::human_bytes, which formats the same
// numbers server-side on the same page. A third copy of this lived in Rust
// and disagreed with both.
function fmtB(b){
  const U=['B','KB','MB','GB','TB'];
  if(b<1024)return b+' B';
  let v=b,u=0;
  while(v>=1024&&u<U.length-1){v/=1024;u++;}
  return v.toFixed(1)+' '+U[u];
}
function rawUrl(path){
  return LIVE_SERVER ? '/api/raw?path='+encodeURIComponent(path) : 'file://'+path;
}
function buildPreview(f){
  var ext=f.ext,path=f.path;
  var metaAttr=escA(JSON.stringify(f.meta));
  if(ext==='jpg'||ext==='jpeg'||ext==='png'||ext==='gif'||ext==='webp'||ext==='bmp'){
    var url=rawUrl(path);
    return '<a href="'+escA(url)+'" target="_blank" data-lb-url="'+escA(url)+'" data-lb-type="image" '+
      'data-lb-meta="'+metaAttr+'">'+
      '<img src="'+escA(url)+'" class="thumb" loading="lazy" '+
      'onerror="this.parentElement.innerHTML=\'<span class=no-prev>no preview</span>\'"></a>';
  }
  if(ext==='heic'){
    if(LIVE_SERVER){
      var thumbUrl=rawUrl(path)+'&size=240';
      var lbUrl=rawUrl(path)+'&size=1200';
      return '<img src="'+escA(thumbUrl)+'" class="thumb heic-loading" loading="lazy" data-lb-url="'+escA(lbUrl)+'" '+
        'data-lb-type="image" data-lb-meta="'+metaAttr+'" '+
        'onload="this.classList.remove(\'heic-loading\')" '+
        'onerror="this.parentElement.innerHTML=\'<span class=no-prev>no preview</span>\'">';
    }
    if(f.tb){
      var src='data:image/jpeg;base64,'+f.tb;
      var lb=f.fb?'data:image/jpeg;base64,'+f.fb:src;
      return '<img src="'+src+'" class="thumb" data-lb-url="'+escA(lb)+'" data-lb-type="image" '+
        'data-lb-meta="'+metaAttr+'">';
    }
    return '<span class="no-prev">HEIC</span>';
  }
  if(ext==='tiff')return '<span class="no-prev">TIFF</span>';
  if(ext==='dng') return '<span class="no-prev">DNG</span>';
  if(ext==='mov'||ext==='mp4'){
    var url=rawUrl(path);
    return '<video src="'+escA(url)+'" class="thumb" preload="metadata" muted playsinline '+
      'data-lb-url="'+escA(url)+'" data-lb-type="video" '+
      'data-lb-meta="'+metaAttr+'" '+
      'onerror="this.outerHTML=\'<span class=no-prev>no preview</span>\'"></video>';
  }
  return '<span class="no-prev">&mdash;</span>';
}
function buildRow(f,isKeep){
  var rc=isKeep?'keep':'remove';
  var bc=isKeep?'keep-badge':'remove-badge';
  var bt=isKeep?'KEEP':'REMOVE';
  var fname=f.path.split('/').pop()||f.path;
  var cr=f.cr||'<span class="dim">—</span>';
  var mo=f.mo||'<span class="dim">—</span>';
  var ex=f.ex||'<span class="dim">—</span>';
  var gps='<span class="dim">—</span>';
  if(f.lat!=null&&f.lon!=null){
    gps='<div class="gps"><a href="https://maps.google.com/?q='+f.lat.toFixed(6)+','+f.lon.toFixed(6)+
      '" target="_blank" rel="noopener">'+Math.abs(f.lat).toFixed(4)+'&deg;'+(f.lat>=0?'N':'S')+' '+
      Math.abs(f.lon).toFixed(4)+'&deg;'+(f.lon>=0?'E':'W')+'</a></div>';
  }
  var dims=(f.w&&f.h)?f.w+'×'+f.h:'<span class="dim">—</span>';
  return '<tr class="'+rc+'">'+
    '<td class="preview">'+buildPreview(f)+'</td>'+
    '<td class="badge"><span class="'+bc+'">'+bt+'</span>'+similarBtn(f.hash)+'</td>'+
    '<td class="filename" title="'+escA(fname)+'">'+escH(fname)+'</td>'+
    '<td class="path-cell"><span class="path-text">'+escH(f.path)+'</span>'+
    '<button class="copy-btn" data-path="'+escA(f.path)+'" title="Copy path">&#x2398;</button></td>'+
    '<td>'+fmtB(f.size)+'</td>'+
    '<td class="dim">'+cr+'</td>'+
    '<td class="dim">'+mo+'</td>'+
    '<td class="dim">'+ex+'</td>'+
    '<td>'+gps+'</td>'+
    '<td class="dim">'+dims+'</td>'+
    '</tr>';
}
function buildGroup(g,idx){
  var rows=g.files.map(function(f,j){return buildRow(f,j===0);}).join('');
  return '<div class="group" id="g'+idx+'">'+
    '<div class="group-header">'+
    '<span class="arrow">&#9654;</span>'+
    '<code class="hash">'+escH(g.hash)+'</code>'+
    '<span class="group-meta">'+g.files.length+' copies &middot; '+fmtB(g.files[0].size)+' each</span>'+
    '<span class="waste">&minus;'+fmtB(g.waste)+' wasted</span>'+
    '</div>'+
    '<div class="group-body">'+
    '<table><thead><tr>'+
    '<th class="preview-th">Preview</th>'+
    '<th>Status</th><th>Filename</th><th>Path</th>'+
    '<th>Size</th><th>Created</th><th>Modified</th><th>EXIF date</th>'+
    '<th>GPS</th><th>Dimensions</th>'+
    '</tr></thead><tbody>'+rows+'</tbody></table></div></div>';
}
function render(reset){
  var overlay=document.getElementById('sort-overlay');
  var container=document.getElementById('groups-container');
  if(!container){if(overlay)overlay.style.display='none';return;}
  if(reset){shown=0;container.innerHTML='';}
  var end=Math.min(shown+PAGE,sorted.length);
  var html='';
  for(var i=shown;i<end;i++)html+=buildGroup(sorted[i],i);
  var tmp=document.createElement('div');
  tmp.innerHTML=html;
  while(tmp.firstChild)container.appendChild(tmp.firstChild);
  shown=end;
  updateBtn();
  overlay.style.display='none';
}
function updateBtn(){
  var btn=document.getElementById('more-btn');
  if(!btn)return;
  var rem=sorted.length-shown;
  if(rem>0){btn.style.display='inline-block';btn.textContent='Show more ('+rem+' remaining)';}
  else btn.style.display='none';
}
function showMore(){render(false);}
function toggle(id){
  var g=document.getElementById(id);
  g.classList.toggle('open');
  if(g.classList.contains('open')){
    g.querySelectorAll('img').forEach(function(img){if(img.loading==='lazy')img.loading='eager';});
    g.querySelectorAll('video').forEach(function(v){if(v.preload==='metadata')v.preload='auto';});
  }
}
function expandAll(){
  document.querySelectorAll('.group').forEach(function(g){
    g.classList.add('open');
    g.querySelectorAll('img').forEach(function(img){if(img.loading==='lazy')img.loading='eager';});
    g.querySelectorAll('video').forEach(function(v){if(v.preload==='metadata')v.preload='auto';});
  });
}
function collapseAll(){document.querySelectorAll('.group').forEach(function(g){g.classList.remove('open');});}
function copyPath(p){
  navigator.clipboard.writeText(p).catch(function(){
    var t=document.createElement('textarea');t.value=p;
    document.body.appendChild(t);t.select();document.execCommand('copy');
    document.body.removeChild(t);
  });
}
function renderMetaPanel(meta){
  var el = document.getElementById('lbMeta');
  if(!meta || (!meta.faces.length && !meta.location)){
    el.classList.remove('on'); el.innerHTML=''; return;
  }
  var parts = [];
  if(meta.faces.length){
    // A live page carries `id` and fetches the crop from the endpoint only when
    // this lightbox opens. A static export has no server to ask, so it carries
    // `thumb` as a data URI. Supporting both keeps one renderer for both.
    parts.push(meta.faces.map(function(fc){
      var src = fc.thumb ? escA(fc.thumb) : '/api/face-image/'+encodeURIComponent(fc.id);
      return '<div class="lb-face"><img src="'+src+'" loading="lazy">'+
        '<a href="/person/'+encodeURIComponent(fc.name)+'?from=lightbox">'+escH(fc.name)+'</a></div>';
    }).join(''));
  }
  if(meta.location){
    var locId = 'lbLoc'+Math.random().toString(36).slice(2);
    parts.push('<div class="lb-location" id="'+locId+'">Loading location...</div>');
    fetch('/api/location?lat='+meta.location.lat+'&lon='+meta.location.lon)
      .then(function(r){ return r.json(); })
      .then(function(d){
        var n = document.getElementById(locId);
        if(n) n.textContent = d.name || 'Unknown location';
      })
      .catch(function(){
        var n = document.getElementById(locId);
        if(n) n.textContent = 'Location unavailable';
      });
  }
  el.innerHTML = parts.join('');
  el.classList.add('on');
}
function openLb(url,type,metaJson){
  var meta = null;
  try { meta = metaJson ? JSON.parse(metaJson) : null; } catch(e) {}
  renderMetaPanel(meta);
  var img=document.getElementById('lb-img');
  var vid=document.getElementById('lb-vid');
  if(type==='video'){
    img.style.display='none';vid.style.display='block';
    vid.src=url;vid.play();
  } else {
    vid.style.display='none';img.style.display='block';img.src=url;
  }
  document.getElementById('lb').classList.add('on');
}
function closeLb(){
  var vid=document.getElementById('lb-vid');
  vid.pause();vid.src='';
  document.getElementById('lb-img').src='';
  document.getElementById('lb').classList.remove('on');
}
function sortGroups(by){
  var overlay=document.getElementById('sort-overlay');
  overlay.style.display='flex';
  requestAnimationFrame(function(){
    requestAnimationFrame(function(){
      sorted.sort(function(a,b){
        if(by==='waste')return b.waste-a.waste;
        var da=a.date||'￿',db=b.date||'￿';
        return by==='date-asc'?da.localeCompare(db):db.localeCompare(da);
      });
      render(true);
    });
  });
}
function bestDateBucket(f){
  var d = bestDateJs(f);
  if(!d) return null;
  return {year: d.slice(0,4), month: d.slice(0,7), day: d.slice(0,10)};
}
var dateState = {level:'year', year:null, month:null};
function dateKeepFiles(){ return (typeof KEEPFILES!=='undefined') ? KEEPFILES : []; }
function buildYearView(){
  dateState = {level:'year', year:null, month:null};
  var byYear = {};
  dateKeepFiles().forEach(function(f){
    var b = bestDateBucket(f); if(!b) return;
    (byYear[b.year] = byYear[b.year] || []).push(f);
  });
  var years = Object.keys(byYear).sort().reverse();
  var grid = document.getElementById('dateGrid');
  grid.innerHTML = years.map(function(y){
    var files = byYear[y];
    return '<div class="date-card" data-year="'+y+'" onclick="buildMonthView(\''+y+'\')">'+
      buildPreview(files[0])+
      '<div class="date-card-label">'+y+'</div>'+
      '<div class="date-card-count">'+files.length+' files</div></div>';
  }).join('');
  document.getElementById('dateBreadcrumb').innerHTML = '';
}
function buildMonthView(year){
  dateState = {level:'month', year:year, month:null};
  var byMonth = {};
  dateKeepFiles().forEach(function(f){
    var b = bestDateBucket(f); if(!b || b.year!==year) return;
    (byMonth[b.month] = byMonth[b.month] || []).push(f);
  });
  var months = Object.keys(byMonth).sort().reverse();
  var grid = document.getElementById('dateGrid');
  grid.innerHTML = months.map(function(m){
    var files = byMonth[m];
    return '<div class="date-card" data-month="'+m+'" onclick="buildDayView(\''+m+'\')">'+
      buildPreview(files[0])+
      '<div class="date-card-label">'+m+'</div>'+
      '<div class="date-card-count">'+files.length+' files</div></div>';
  }).join('');
  document.getElementById('dateBreadcrumb').innerHTML =
    '<a onclick="buildYearView()">'+year+'</a>';
}
function buildDayView(month){
  dateState = {level:'day', year:dateState.year, month:month};
  var byDay = {};
  dateKeepFiles().forEach(function(f){
    var b = bestDateBucket(f); if(!b || b.month!==month) return;
    (byDay[b.day] = byDay[b.day] || []).push(f);
  });
  var days = Object.keys(byDay).sort().reverse();
  var grid = document.getElementById('dateGrid');
  grid.innerHTML = days.map(function(d){
    var files = byDay[d];
    return '<div class="date-card" data-day="'+d+'" onclick="buildDayGallery(\''+d+'\')">'+
      buildPreview(files[0])+
      '<div class="date-card-label">'+d+'</div>'+
      '<div class="date-card-count">'+files.length+' files</div></div>';
  }).join('');
  document.getElementById('dateBreadcrumb').innerHTML =
    '<a onclick="buildYearView()">'+dateState.year+'</a> &gt; '+
    '<a onclick="buildMonthView(\''+dateState.year+'\')">'+month+'</a>';
}
function buildDayGallery(day){
  var files = dateKeepFiles().filter(function(f){
    var b = bestDateBucket(f); return b && b.day===day;
  });
  var grid = document.getElementById('dateGrid');
  grid.innerHTML = files.map(function(f){ return buildCard(f); }).join('');
  document.getElementById('dateBreadcrumb').innerHTML =
    '<a onclick="buildYearView()">'+dateState.year+'</a> &gt; '+
    '<a onclick="buildMonthView(\''+dateState.year+'\')">'+dateState.month+'</a> &gt; '+day;
}
// Event delegation: toggle, lightbox, copy. One listener for all dynamic content
document.addEventListener('click',function(e){
  var lb=e.target.closest('[data-lb-url]');
  if(lb){e.preventDefault();e.stopPropagation();openLb(lb.dataset.lbUrl,lb.dataset.lbType||'image',lb.dataset.lbMeta);return;}
  var cp=e.target.closest('[data-path]');
  if(cp){copyPath(cp.dataset.path);return;}
  var hdr=e.target.closest('.group-header');
  if(hdr){toggle(hdr.closest('.group').id);return;}
});
document.addEventListener('keydown',function(e){if(e.key==='Escape')closeLb();});
document.getElementById('lb').addEventListener('click',function(e){
  if(e.target===this)closeLb();
});

// ---- All-files gallery and similarity search (active only with --all) ----
var GPAGE=200,gShown=0,HASH_FILES={},VECS=null,VEC_INDEX={};
function decodeVecs(b64,n,dim){
  var bin=atob(b64);
  var out=new Float32Array(n*dim);
  for(var i=0;i<n*dim;i++){
    var lo=bin.charCodeAt(i*2),hi=bin.charCodeAt(i*2+1);
    var h=(hi<<8)|lo;
    var s=(h&0x8000)?-1:1,e=(h>>10)&0x1f,f=h&0x3ff;
    if(e===0)out[i]=s*f*Math.pow(2,-24);
    else if(e===31)out[i]=f?NaN:s*Infinity;
    else out[i]=s*(1+f/1024)*Math.pow(2,e-15);
  }
  return out;
}
function bestDateJs(f){
  if(f.ex&&f.ex.indexOf('0000')!==0)return f.ex;
  if(f.cr&&f.mo)return f.cr<f.mo?f.cr:f.mo;
  return f.cr||f.mo||'';
}
function similarBtn(hash){
  if(!VECS||VEC_INDEX[hash]==null)return '';
  return '<button class="similar-btn" data-similar="'+escA(hash)+'">Similar</button>';
}
function buildCard(f){
  var fname=f.path.split('/').pop()||f.path;
  // `copies` arrives on the row from /api/files. An inlined page has no such
  // field and falls back to counting the array, which is the only reason that
  // array ever had to be complete.
  var n = (typeof f.copies==='number') ? f.copies
        : (HASH_FILES[f.hash] ? HASH_FILES[f.hash].length : 1);
  var copies = n>1 ? '<span class="copies">x'+n+'</span>' : '';
  return '<div class="card" data-hash="'+escA(f.hash)+'">'+copies+
    buildPreview(f)+
    '<div class="card-meta" title="'+escA(f.path)+'">'+escH(fname)+'</div>'+
    '<div class="card-meta">'+fmtB(f.size)+(bestDateJs(f)?' &middot; '+escH(bestDateJs(f)):'')+'</div>'+
    similarBtn(f.hash)+
    '</div>';
}
// Appends one page of cards and updates the button. Shared by both paths, so an
// inlined page and a fetched one render identically.
function appendCards(files,total){
  var g=document.getElementById('gallery');
  var html='';
  for(var i=0;i<files.length;i++)html+=buildCard(files[i]);
  var tmp=document.createElement('div');
  tmp.innerHTML=html;
  while(tmp.firstChild)g.appendChild(tmp.firstChild);
  gShown+=files.length;
  var btn=document.getElementById('gallery-more');
  var rem=total-gShown;
  if(rem>0){btn.style.display='inline-block';btn.textContent='Show more ('+rem+' remaining)';}
  else btn.style.display='none';
}
// Guards a second fetch while one is in flight; a double-click on "Show more"
// would otherwise append the same page twice.
var gLoading=false;
function renderGallery(){
  if(typeof ALLFILES!=='undefined'){
    appendCards(ALLFILES.slice(gShown,gShown+GPAGE),ALLFILES.length);
    return;
  }
  if(gLoading)return;
  gLoading=true;
  var btn=document.getElementById('gallery-more');
  if(btn)btn.textContent='Loading\u2026';
  fetch('/api/files?view='+encodeURIComponent(GVIEW)+'&offset='+gShown+'&limit='+GPAGE)
    .then(function(r){ return r.json(); })
    .then(function(d){ gLoading=false; appendCards(d.files||[],d.total||0); })
    .catch(function(){
      gLoading=false;
      // Say so, rather than leaving a button that silently does nothing.
      if(btn)btn.textContent='Could not load more. Click to retry.';
    });
}
function showMoreGallery(){renderGallery();}
function findSimilar(hash){
  var qi=VEC_INDEX[hash];
  if(qi==null||!VECS)return;
  var q=VECS.subarray(qi*VEC_DIM,(qi+1)*VEC_DIM);
  var scores=[];
  for(var i=0;i<VEC_HASHES.length;i++){
    if(i===qi)continue;
    var v=VECS.subarray(i*VEC_DIM,(i+1)*VEC_DIM);
    var dot=0;
    for(var d=0;d<VEC_DIM;d++)dot+=q[d]*v[d];
    if(isFinite(dot))scores.push([i,dot]);
  }
  scores.sort(function(a,b){return b[1]-a[1];});
  renderResults(hash,scores.slice(0,24));
}
function resultCard(hash,score,isQuery){
  var files=HASH_FILES[hash];
  if(!files||!files.length)return '';
  var f=files[0];
  var fname=f.path.split('/').pop()||f.path;
  var badge=isQuery?'':'<span class="score">'+score.toFixed(3)+'</span>';
  var copies=files.length>1?'<span class="copies">x'+files.length+'</span>':'';
  return '<div class="rcard'+(isQuery?' query':'')+'" data-hash="'+escA(hash)+'">'+
    badge+copies+buildPreview(f)+
    '<div class="rname" title="'+escA(f.path)+'">'+(isQuery?'query: ':'')+escH(fname)+'</div>'+
    '</div>';
}
// :warning: Results are drawn from rows fetched by hash, not from a complete
// in-memory array. Similarity ranks a few dozen out of the whole library, and
// requiring every row to be present just to display 24 of them is what made the
// page carry the library in the first place.
function renderResults(qHash,scored){
  var need=[qHash];
  for(var i=0;i<scored.length;i++)need.push(VEC_HASHES[scored[i][0]]);
  var missing=need.filter(function(h){return !HASH_FILES[h];});
  if(missing.length===0){ drawResults(qHash,scored); return; }
  fetch('/api/files?hashes='+encodeURIComponent(missing.join(',')))
    .then(function(r){ return r.json(); })
    .then(function(d){
      (d.files||[]).forEach(function(f){
        (HASH_FILES[f.hash]=HASH_FILES[f.hash]||[]).push(f);
      });
      drawResults(qHash,scored);
    })
    .catch(function(){ drawResults(qHash,scored); });
}
function drawResults(qHash,scored){
  var panel=document.getElementById('results');
  var html='<div class="results-head"><h2>Similar images</h2>'+
    '<button onclick="clearResults()">Clear</button></div>'+
    '<div class="results-strip">'+resultCard(qHash,1,true);
  for(var i=0;i<scored.length;i++){
    html+=resultCard(VEC_HASHES[scored[i][0]],scored[i][1],false);
  }
  html+='</div>';
  panel.innerHTML=html;
  panel.style.display='block';
  panel.querySelectorAll('img').forEach(function(img){if(img.loading==='lazy')img.loading='eager';});
  panel.scrollIntoView({behavior:'smooth',block:'start'});
}
function clearResults(){
  var panel=document.getElementById('results');
  panel.style.display='none';
  panel.innerHTML='';
}
if(typeof ALLFILES!=='undefined'){
  ALLFILES.forEach(function(f){
    (HASH_FILES[f.hash]=HASH_FILES[f.hash]||[]).push(f);
  });
  if(VEC_HASHES.length>0){
    VECS=decodeVecs(VEC_B64,VEC_HASHES.length,VEC_DIM);
    for(var vi=0;vi<VEC_HASHES.length;vi++)VEC_INDEX[VEC_HASHES[vi]]=vi;
  }
  renderGallery();
}else if(document.getElementById('gallery')){
  // Nothing inlined: a live page above the vector gate. Fetch the first page.
  renderGallery();
}
document.addEventListener('click',function(e){
  var sb=e.target.closest('[data-similar]');
  if(sb){e.preventDefault();e.stopPropagation();findSimilar(sb.dataset.similar);}
});
render(true);
if(typeof KEEPFILES!=='undefined') buildYearView();
