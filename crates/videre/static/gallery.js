
// Duplicate groups per Show more: routes.duplicates.pageSize.
var PAGE=settingIntInRange('routes.duplicates.pageSize',1,1000),sorted=GROUPS.slice(),shown=0;

// Sort state, kept in the library's gallery settings the same way the view
// mode is, default date desc. File grids mirror the server whitelist (six
// fields); Library, Date and an event's page share `routes.files.sort`, the
// map keeps its own. The Events overview orders events, by its own fields.
var FILE_SORT_FIELDS=['date','name','size','rating','liked','type'];
var EVENT_SORT_FIELDS=['date','files','length','name'];
var SORT=(function(){
  if(document.getElementById('map-plot'))return{key:'routes.map.sort',fields:FILE_SORT_FIELDS};
  if(typeof GVIEW!=='undefined'&&GVIEW==='events'&&!(typeof GEVENT==='object'&&GEVENT))
    return{key:'routes.events.sort',fields:EVENT_SORT_FIELDS};
  return{key:'routes.files.sort',fields:FILE_SORT_FIELDS};
})();
var rerunDateView=null;  // set by each date renderer, so a sort change re-runs the view on screen
var allFilesSorted=null; // ALLFILES, re-sorted per sort choice (static export only)
function sortState(){
  return{
    field:settingOneOf(SORT.key+'.field',SORT.fields),
    dir:settingOneOf(SORT.key+'.dir',['asc','desc'])
  };
}
function storeSortState(s){ saveSetting(SORT.key,{field:s.field,dir:s.dir}); }
function sortQuery(){ var s=sortState(); return '&sort='+s.field+'&dir='+s.dir; }
function setSortField(f){
  var s=sortState(); s.field=f; storeSortState(s);
  reflectSort(); refetchCurrentView();
}
function toggleSortDir(){
  var s=sortState(); s.dir=(s.dir==='asc'?'desc':'asc'); storeSortState(s);
  reflectSort(); refetchCurrentView();
}
function reflectSort(){
  var s=sortState();
  document.querySelectorAll('.sort-select').forEach(function(sel){ sel.value=s.field; });
  document.querySelectorAll('.sort-dir-btn').forEach(function(btn){
    btn.setAttribute('aria-pressed',s.dir==='desc'?'true':'false');
  });
}
function basenameOf(p){ var i=p.lastIndexOf('/'); return i<0?p:p.slice(i+1); }
function cmpPath(a,b){ return a.path<b.path?-1:(a.path>b.path?1:0); }
// The static comparator: the exact semantics of the server's ORDER BY (see
// the gallery sort spec). Values compare with the direction; nulls sit last
// in both directions; the path tie-break is direction-neutral. Liked keeps
// effective date, newest first, as its second key, like the SQL.
function sortFiles(files){
  var s=sortState(),f=s.field,asc=(s.dir==='asc');
  function by(fn){
    return function(a,b){
      var va=fn(a),vb=fn(b);
      if(va===null||vb===null)return (va===vb)?0:(va===null?1:-1);
      if(va<vb)return asc?-1:1;
      if(va>vb)return asc?1:-1;
      return 0;
    };
  }
  function dateOrNull(x){ return bestDateJs(x)||null; }
  function nameOf(x){ return basenameOf(x.path).toLowerCase(); }
  function sizeOf(x){ return (typeof x.size==='number')?x.size:null; }
  function ratingOf(x){ return (typeof x.rating==='number')?x.rating:null; }
  function typeOf(x){ return (x.ext==='mov'||x.ext==='mp4')?1:0; }
  var cmp;
  if(f==='date')cmp=by(dateOrNull);
  else if(f==='name')cmp=by(nameOf);
  else if(f==='size')cmp=by(sizeOf);
  else if(f==='rating')cmp=by(ratingOf);
  else if(f==='type')cmp=by(typeOf);
  else{
    cmp=function(a,b){
      var al=a.liked?1:0,bl=b.liked?1:0;
      if(al!==bl)return asc?al-bl:(bl-al);
      var da=dateOrNull(a),db=dateOrNull(b);
      if(da===null||db===null)return (da===db)?0:(da===null?1:-1);
      return da<db?1:(da>db?-1:0);
    };
  }
  return files.slice().sort(function(a,b){
    var r=cmp(a,b);
    return r!==0?r:cmpPath(a,b);
  });
}
function refetchCurrentView(){
  var g=document.getElementById('gallery');
  if(g){
    gRequest++; gLoading=false; gShown=0; galleryFiles=[]; allFilesSorted=null;
    clearTileMode(g); g.innerHTML='';
    renderGallery();
    return;
  }
  if(rerunDateView)rerunDateView();
}

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
// Must move with RASTER_CACHE_FORMAT_VERSION in videre_core::thumb_cache.
// It keeps a browser from reusing preview bytes produced by an older renderer.
var RASTER_PREVIEW_VERSION='raster-v1';
function rawUrl(f, size, version){
  if(!LIVE_SERVER) return 'file://'+f.path;
  var query=size?('?size='+size):'';
  if(version)query+='&preview='+encodeURIComponent(version);
  return '/api/files/'+encodeURIComponent(f.hash)+'/raw'+(query?query:'');
}
function buildPreview(f){
  var ext=f.ext,path=f.path;
  // The lightbox info bar also shows the filename and size, which live on the
  // file row rather than in its `meta`, so fold them in here.
  var metaAttr=escA(JSON.stringify(Object.assign({}, f.meta, {
    name: (f.path.split('/').pop()||f.path),
    size: f.size,
    date: bestDateJs(f),
    ext: f.ext,
    hash: f.hash,
    liked: !!f.liked
  })));
  if(ext==='jpg'||ext==='jpeg'||ext==='png'||ext==='gif'||ext==='webp'||ext==='bmp'){
    // Grid/tile thumbnails are server-downscaled, not the full original:
    // serving originals as tiles saturates the browser's connection pool on
    // a large library and most tiles never load. 480px keeps justified-row
    // tiles (220 CSS px) sharp on a 2x display; the lightbox gets a larger
    // 1200px render; the link still points at the full original.
    var thumbUrl=rawUrl(f,480,RASTER_PREVIEW_VERSION);
    var lbUrl=rawUrl(f,1200,RASTER_PREVIEW_VERSION);
    var full=rawUrl(f);
    return '<a href="'+escA(full)+'" target="_blank" data-lb-url="'+escA(lbUrl)+'" data-lb-type="image" '+
      'data-lb-meta="'+metaAttr+'">'+
      '<img src="'+escA(thumbUrl)+'" class="thumb" loading="lazy" '+
      'onerror="this.parentElement.innerHTML=\'<span class=no-prev>no preview</span>\'"></a>';
  }
  if(ext==='heic'){
    if(LIVE_SERVER){
      var thumbUrl=rawUrl(f, 480);
      var lbUrl=rawUrl(f, 1200);
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
    var url=rawUrl(f);
    // A play badge marks the tile as a video: on a live macOS server the tile is
    // an oriented poster frame (rotation baked in by QuickLook, so it can't
    // render sideways) that otherwise looks like a photo. Clicking plays the
    // video in the lightbox via data-lb-type=video. On error the whole badged
    // wrapper collapses to "no preview". Elsewhere (Linux, or a static export)
    // the plain inline <video> is kept, badged the same way.
    var badge='<span class="vbadge" aria-hidden="true"></span>';
    if(typeof VIDEO_POSTERS!=='undefined'&&VIDEO_POSTERS){
      var poster=rawUrl(f,480);
      return '<span class="vthumb">'+
        '<img src="'+escA(poster)+'" class="thumb" loading="lazy" '+
        'data-lb-url="'+escA(url)+'" data-lb-type="video" data-lb-meta="'+metaAttr+'" '+
        'onerror="this.parentElement.outerHTML=\'<span class=no-prev>no preview</span>\'">'+
        badge+'</span>';
    }
    return '<span class="vthumb">'+
      '<video src="'+escA(url)+'" class="thumb" preload="metadata" muted playsinline '+
      'data-lb-url="'+escA(url)+'" data-lb-type="video" data-lb-meta="'+metaAttr+'" '+
      'onerror="this.parentElement.outerHTML=\'<span class=no-prev>no preview</span>\'"></video>'+
      badge+'</span>';
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
    // A Google Takeout pair: the original (kept, first) and Google's edit.
    (g.edited
      ? '<span class="hash">edited in Google Photos</span>'+
        '<span class="group-meta">original and edit</span>'
      : '<code class="hash">'+escH(g.hash)+'</code>'+
        '<span class="group-meta">'+g.files.length+' copies &middot; '+fmtB(g.files[0].size)+' each</span>')+
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
  if(!sorted.length&&GQUERY)container.innerHTML='<p class="muted">No duplicate group has a matching file.</p>';
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
// Opening a group leaves its images lazy: they load as they near the screen.
// Turning them eager made Expand all request every thumbnail on the page at
// once, a queue that kept later requests waiting.
function toggle(id){
  document.getElementById(id).classList.toggle('open');
}
function expandAll(){
  document.querySelectorAll('.group').forEach(function(g){ g.classList.add('open'); });
}
function collapseAll(){document.querySelectorAll('.group').forEach(function(g){g.classList.remove('open');});}
function copyPath(p){
  navigator.clipboard.writeText(p).catch(function(){
    var t=document.createElement('textarea');t.value=p;
    document.body.appendChild(t);t.select();document.execCommand('copy');
    document.body.removeChild(t);
  });
}
// Small leading icons for the info rows (stroke uses currentColor, sized in CSS).
const ICON_FILE='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6"/></svg>';
const ICON_DATE='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="18" rx="2"/><path d="M8 2v4M16 2v4M3 10h18"/></svg>';
const ICON_SIZE='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6"/><path d="M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/></svg>';
const ICON_PIN='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 10c0 6-8 12-8 12s-8-6-8-12a8 8 0 0 1 16 0z"/><circle cx="12" cy="10" r="3"/></svg>';
// Heart for the like toggle. One path; the CSS decides outline vs filled red by
// the button's `liked` class (fill:none + stroke, or fill:red).
const ICON_HEART='<svg viewBox="0 0 24 24" stroke-linejoin="round" stroke-linecap="round"><path d="M12 21C12 21 3 14.5 3 8.5 3 5.42 5.42 3 8.5 3c1.74 0 3.41 0.81 4.5 2.09C14.09 3.81 15.76 3 17.5 3 20.58 3 23 5.42 23 8.5 23 14.5 12 21 12 21Z" transform="translate(-1 0)"/></svg>';
// Selection bar icons: Feather (https://feathericons.com), MIT License,
// Copyright (c) 2013-2023 Cole Bemis. 24px viewBox, stroked in currentColor.
function selIcon(d){ return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">'+d+'</svg>'; }
const ICON_STAR=selIcon('<polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2"/>');
const ICON_LABEL=selIcon('<path d="M12 2.69l5.66 5.66a8 8 0 1 1-11.31 0z"/>');
const ICON_TAG=selIcon('<path d="M20.59 13.41l-7.17 7.17a2 2 0 0 1-2.83 0L2 12V2h10l8.59 8.59a2 2 0 0 1 0 2.82z"/><line x1="7" y1="7" x2="7.01" y2="7"/>');
const ICON_FLAG=selIcon('<path d="M4 15s1-1 4-1 5 2 8 2 4-1 4-1V3s-1 1-4 1-5-2-8-2-4 1-4 1z"/><line x1="4" y1="22" x2="4" y2="15"/>');
const ICON_ROT_L=selIcon('<polyline points="1 4 1 10 7 10"/><path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10"/>');
const ICON_ROT_R=selIcon('<polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/>');
const ICON_MORE=selIcon('<circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/>');
const ICON_UP=selIcon('<line x1="12" y1="19" x2="12" y2="5"/><polyline points="5 12 12 5 19 12"/>');
const ICON_COPY=selIcon('<rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>');
const ICON_CHECK=selIcon('<polyline points="20 6 9 17 4 12"/>');
const ICON_TRASH=selIcon('<polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/><line x1="10" y1="11" x2="10" y2="17"/><line x1="14" y1="11" x2="14" y2="17"/>');
function renderMetaPanel(meta){
  const el = document.getElementById('lbMeta');
  // Shown under the media. The left column carries the file facts; a right
  // column of people is added only when the file has labeled faces, so a file
  // with none stays a single full-width column.
  el.classList.add('on');
  if(!meta){ el.innerHTML=''; return; }
  const rows = [];
  if(meta.name) rows.push('<div class="lb-row lb-fname">'+ICON_FILE+'<span>'+escH(meta.name)+'</span>'+
    '<button type="button" class="lb-copy" data-copy-text="'+escA(meta.name)+'" title="Copy file name" aria-label="Copy file name">'+ICON_COPY+'</button></div>');
  if(meta.date){
    // On a live server the date links to its day view; a static export has no
    // such route, so it stays plain text.
    const dm = /^(\d{4})-(\d{2})-(\d{2})/.exec(String(meta.date));
    const dLabel = escH(humanDate(meta.date));
    const dInner = (LIVE_SERVER && dm)
      ? '<a class="lb-link" href="/date/'+dm[1]+'/'+dm[2]+'/'+dm[3]+'">'+dLabel+'</a>'
      : dLabel;
    rows.push('<div class="lb-row">'+ICON_DATE+'<span>'+dInner+'</span></div>');
  }
  if(meta.size!=null) rows.push('<div class="lb-row">'+ICON_SIZE+'<span>'+fmtB(meta.size)+'</span></div>');
  if(meta.location){
    const locId = 'lbLoc'+Math.random().toString(36).slice(2);
    rows.push('<div class="lb-row lb-location">'+ICON_PIN+'<span id="'+locId+'">Loading location...</span></div>');
    fetch('/api/locations?lat='+meta.location.lat+'&lon='+meta.location.lon)
      .then(r => r.json())
      .then(d => {
        const n = document.getElementById(locId);
        if(!n) return;
        if(d.name){
          // On a live server the place links to the map by the photo's own
          // coordinates, not by this reverse-geocoded name: the name is finer
          // ("Schöneberg, DE") than any cluster's ("Berlin"), so the server
          // resolves the point to the nearest cluster and selects its tag. A
          // static export has no server, so it stays plain text.
          n.innerHTML = LIVE_SERVER
            ? '<a class="lb-link" href="/map?near='+encodeURIComponent(meta.location.lat+','+meta.location.lon)+'">'+escH(d.name)+'</a>'
            : escH(d.name);
        } else {
          n.textContent = 'Unknown location';
        }
      })
      .catch(() => { const n = document.getElementById(locId); if(n) n.textContent = 'Location unavailable'; });
  }
  // The like toggle sits at the top-left of the metadata section, larger than
  // the row icons. Only on a live server, where the PATCH can persist it; a
  // static export has nothing to save to. Red when liked, outline otherwise.
  let likeBtn = '';
  if(typeof LIVE_SERVER!=='undefined' && LIVE_SERVER && meta.hash){
    const on = !!meta.liked;
    likeBtn = '<button type="button" class="lb-like'+(on?' liked':'')+'" '+
      'data-hash="'+escA(meta.hash)+'" aria-pressed="'+on+'" '+
      'aria-label="Like" title="Like" onclick="toggleLbLike(this)">'+ICON_HEART+'</button>';
  }
  const hasPeople = !!(meta.faces && meta.faces.length);
  let html = '<div class="lb-info">'+likeBtn+rows.join('')+'</div>';
  if(hasPeople){
    // A live page carries `id` and fetches the crop from the endpoint on open;
    // a static export carries `thumb` as a data URI. One renderer serves both.
    const people = meta.faces.map(fc => {
      const src = fc.thumb ? escA(fc.thumb) : '/api/faces/'+encodeURIComponent(fc.id)+'/image';
      // The whole card links to the person, so the photo is clickable too.
      return '<a class="lb-face" href="'+peopleRootG()+'person/'+encodeURIComponent(fc.name)+'?from=lightbox">'+
        '<img src="'+src+'" loading="lazy"><span>'+escH(fc.name)+'</span></a>';
    }).join('');
    html += '<div class="lb-people">'+people+'</div>';
  }
  el.innerHTML = html;
}
// Toggle a photo's like from the lightbox, the same mark `videre mark --like`
// sets. Optimistic: flip the heart immediately, revert if the PATCH fails, and
// keep the open tiles' cached meta in step so reopening shows the new state.
function toggleLbLike(btn){
  var hash=btn.dataset.hash;
  if(!hash)return;
  var next=!btn.classList.contains('liked');
  btn.classList.toggle('liked', next);
  btn.setAttribute('aria-pressed', next);
  syncLikedMeta(hash, next);
  fetch('/api/files/'+encodeURIComponent(hash),{
    method:'PATCH',
    headers:{'content-type':'application/json'},
    body:JSON.stringify({liked:next})
  }).then(function(r){ if(!r.ok)throw new Error('like failed'); })
    .catch(function(){
      btn.classList.toggle('liked', !next);
      btn.setAttribute('aria-pressed', !next);
      syncLikedMeta(hash, !next);
    });
}
// Write the liked flag back into the loaded rows and the tiles' data-lb-meta so
// a later reopen of the same photo reflects it without a page reload.
function syncLikedMeta(hash, liked){
  if(typeof galleryFiles!=='undefined'){
    galleryFiles.forEach(function(f){ if(f.hash===hash) f.liked=liked; });
  }
  if(typeof dateFiles!=='undefined'&&dateFiles){
    dateFiles.forEach(function(f){ if(f.hash===hash) f.liked=liked; });
  }
  document.querySelectorAll('[data-lb-meta]').forEach(function(el){
    try{
      var m=JSON.parse(el.dataset.lbMeta);
      if(m && m.hash===hash){ m.liked=liked; el.dataset.lbMeta=JSON.stringify(m); }
    }catch(e){}
  });
  document.querySelectorAll('.tile[data-hash="'+hash+'"],.card[data-hash="'+hash+'"]').forEach(function(el){
    var host=likedBadgeHost(el);
    if(!host)return;
    var badge=host.querySelector('.liked-badge');
    if(liked&&!badge)host.insertAdjacentHTML('beforeend',likedBadge({liked:true}));
    else if(!liked&&badge)badge.remove();
  });
}
var lbIndex=-1;      // index of the open item among the currently visible tiles
var lbLoading=false; // guards the auto-paginate load so it fires once
// The formats the rotate button can turn (EXIF Orientation, or a HEIC's irot);
// must match videre gallery's rotate endpoint (supports_rotation).
var ROTATABLE_EXTS=['jpg','jpeg','png','tif','tiff','webp','heic','heif'];
var lbCurrent=null;
function openLb(url,type,metaJson){
  var meta = null;
  try { meta = metaJson ? JSON.parse(metaJson) : null; } catch(e) {}
  renderMetaPanel(meta);
  var img=document.getElementById('lb-img');
  var vid=document.getElementById('lb-vid');
  // Fullscreen is for photos: a playing video already has it in its own
  // controls, and two fullscreen buttons on one player is noise.
  document.getElementById('lb-fs').hidden = (type==='video');
  // Rotate is offered only for images the endpoint can turn: it refuses the
  // rest, so the button never appears where it cannot work.
  var ext=(meta&&meta.ext?String(meta.ext):'').toLowerCase();
  var canRotate = type!=='video' && ROTATABLE_EXTS.indexOf(ext)>=0;
  ['lb-rotate','lb-rotate-ccw'].forEach(function(id){
    var b=document.getElementById(id);
    if(b){ b.hidden=!canRotate; b.disabled=false; }
  });
  lbCurrent = { hash: meta&&meta.hash, url: url };
  // A fresh image starts fit-to-screen at the preview resolution; the full-res
  // original is loaded lazily on the first zoom.
  lbz={scale:1,x:0,y:0,full:false};
  img.style.transform='';
  var stage0=img.closest('.lb-stage'); if(stage0)stage0.classList.remove('zooming');
  if(type==='video'){
    img.style.display='none';vid.style.display='block';
    vid.src=url;vid.play();
  } else {
    // Stop any video that was playing, so stepping from a clip to a photo does
    // not leave audio running behind the hidden <video>.
    vid.pause();vid.src='';
    vid.style.display='none';img.style.display='block';img.src=url;
  }
  document.getElementById('lb').classList.add('on');
}
function closeLb(){
  var vid=document.getElementById('lb-vid');
  vid.pause();vid.src='';
  lbz={scale:1,x:0,y:0,full:false};
  var ci=document.getElementById('lb-img');
  ci.style.transform='';
  var cs=ci.closest('.lb-stage'); if(cs)cs.classList.remove('zooming');
  ci.src='';
  // Leave fullscreen on close, so the next open starts grounded (and Escape
  // does not have to be pressed twice to get back to the page).
  if(document.fullscreenElement)document.exitFullscreen();
  document.getElementById('lb').classList.remove('on');
  lbIndex=-1;
}
// Fullscreen the whole lightbox, so the arrows and the info panel stay
// usable at screen size. The glyph stays put; the aria/title text carries
// the state.
function toggleLbFullscreen(){
  var lb=document.getElementById('lb');
  if(document.fullscreenElement){
    document.exitFullscreen();
  } else if(lb.requestFullscreen){
    lb.requestFullscreen();
  }
}
document.addEventListener('fullscreenchange',function(){
  var b=document.getElementById('lb-fs');
  if(!b)return;
  var on=!!document.fullscreenElement;
  b.title=on?'Exit fullscreen':'Fullscreen';
  b.setAttribute('aria-label',b.title);
});
// Append a cache-busting token to a raw-file URL so a re-rendered preview is
// refetched rather than served from the browser cache.
function bustUrl(url,token){
  var clean=url.split('#')[0].replace(/([&?])b=\d+/,'$1'+token);
  if(clean.indexOf('b='+token.split('=')[1])>=0)return clean;
  return clean+(clean.indexOf('?')>=0?'&':'?')+token;
}
// The latest orientation token per hash, so the full-size original loaded on
// zoom is fetched fresh after a rotation too.
var rotatedTokens={};
// Refresh every on-page URL for one hash after its orientation changed: the
// thumbnails, so the grid tile turns with the lightbox, and the URLs a tile
// opens the lightbox or the original with. The browser keeps an image per URL,
// so reopening a tile with its pre-rotation URL showed the old render.
function refreshTilesFor(hash,token){
  rotatedTokens[hash]=token;
  var needle='/api/files/'+encodeURIComponent(hash)+'/raw';
  var imgs=document.querySelectorAll('img');
  for(var i=0;i<imgs.length;i++){
    if(imgs[i].id==='lb-img')continue;
    if(imgs[i].src&&imgs[i].src.indexOf(needle)>=0)imgs[i].src=bustUrl(imgs[i].src,token);
  }
  document.querySelectorAll('[data-lb-url]').forEach(function(el){
    if(el.dataset.lbUrl.indexOf(needle)>=0)el.dataset.lbUrl=bustUrl(el.dataset.lbUrl,token);
  });
  document.querySelectorAll('a[href]').forEach(function(a){
    var href=a.getAttribute('href');
    if(href.indexOf(needle)>=0)a.setAttribute('href',bustUrl(href,token));
  });
}
// Rotate the open photo 90 clockwise. The click is debounced: the button is
// disabled while the request is in flight, so a rapid double-click cannot queue
// two rotations. On success the source EXIF is bumped and its caches dropped, so
// re-requesting the preview with a fresh token renders it upright.
var lbRotating=false;
function rotateLb(dir){
  if(lbRotating||!lbCurrent||!lbCurrent.hash)return;
  dir = (dir==='ccw') ? 'ccw' : 'cw';
  var btns=[document.getElementById('lb-rotate'),document.getElementById('lb-rotate-ccw')];
  lbRotating=true;
  btns.forEach(function(b){ if(b)b.disabled=true; });
  fetch('/api/files/'+encodeURIComponent(lbCurrent.hash)+'/rotate?dir='+dir,{method:'POST'})
    .then(function(r){ if(!r.ok)throw new Error('rotate failed'); return r.json(); })
    .then(function(){
      var token='b='+Date.now();
      lbCurrent.url=bustUrl(lbCurrent.url,token);
      // Zoomed in on the original: refetch that, not the preview.
      document.getElementById('lb-img').src=lbz.full?bustUrl(lbFullUrl(lbCurrent.hash),token):lbCurrent.url;
      refreshTilesFor(lbCurrent.hash,token);
    })
    .catch(function(){})
    .then(function(){ lbRotating=false; btns.forEach(function(b){ if(b)b.disabled=false; }); });
}

// ---- Lightbox zoom & pan --------------------------------------------------
// The lightbox preview is a downscaled render (1200px), so fit-to-screen never
// shows a large photo at full detail. Clicking the image (or wheeling over it)
// zooms in: the full-resolution original is loaded once and can be panned, so
// the user can inspect it 1:1. The stage grows to a large viewport while zoomed
// so there is room to pan, and the metadata bar hides to give the image space.
var LB_MIN_ZOOM=1, LB_MAX_ZOOM=8;
var lbz={scale:1,x:0,y:0,full:false};
function lbImg(){ return document.getElementById('lb-img'); }
function lbApply(){
  var img=lbImg();
  img.style.transform = lbz.scale===1 ? '' : 'translate('+lbz.x+'px,'+lbz.y+'px) scale('+lbz.scale+')';
  var stage=img.closest('.lb-stage');
  if(stage)stage.classList.toggle('zooming', lbz.scale>1);
}
function lbResetZoom(){
  lbz={scale:1,x:0,y:0,full:lbz.full};
  var img=lbImg();
  img.style.transform='';
  var stage=img.closest('.lb-stage');
  if(stage)stage.classList.remove('zooming');
}
// Swap in the full-resolution original the first time we zoom, so detail is
// actually there to see. Kept for the life of this lightbox open.
function lbLoadFull(){
  if(lbz.full||!lbCurrent||!lbCurrent.hash)return;
  lbz.full=true;
  lbImg().src=lbFullUrl(lbCurrent.hash);
}
function lbFullUrl(hash){
  var url='/api/files/'+encodeURIComponent(hash)+'/raw';
  return rotatedTokens[hash]?bustUrl(url,rotatedTokens[hash]):url;
}
function lbClampPan(){
  var img=lbImg(), stage=img.closest('.lb-stage');
  if(!stage)return;
  var ir=img.getBoundingClientRect(), sr=stage.getBoundingClientRect();
  var maxX=Math.max(0,(ir.width-sr.width)/2+16);
  var maxY=Math.max(0,(ir.height-sr.height)/2+16);
  lbz.x=Math.max(-maxX,Math.min(maxX,lbz.x));
  lbz.y=Math.max(-maxY,Math.min(maxY,lbz.y));
}
// Zoom to `scale`, keeping the point (cx,cy) under the cursor fixed.
function lbZoomAt(scale,cx,cy){
  scale=Math.max(LB_MIN_ZOOM,Math.min(LB_MAX_ZOOM,scale));
  var img=lbImg();
  if(img.style.display==='none')return; // videos do not zoom
  var s0=lbz.scale;
  if(scale===s0)return;
  if(scale>1)lbLoadFull();
  var r=img.getBoundingClientRect();
  // Untransformed centre = current transformed centre minus the translate.
  var cX=(r.left+r.width/2)-lbz.x, cY=(r.top+r.height/2)-lbz.y;
  var dx=cx-cX, dy=cy-cY, k=scale/s0;
  lbz.x=dx-(dx-lbz.x)*k;
  lbz.y=dy-(dy-lbz.y)*k;
  lbz.scale=scale;
  if(scale===1){ lbz.x=0; lbz.y=0; }
  lbApply();
  lbClampPan();
  lbApply();
}
function lbToggleZoom(cx,cy){
  if(lbz.scale>1) lbZoomAt(1,cx,cy);
  else lbZoomAt(2.5,cx,cy);
}
// Prev/next across the visible tiles in DOM order. Every view and the static
// export renders its tiles with data-lb-url, so one walk covers them all; a
// tile hidden inside a collapsed group (offsetParent === null) is skipped.
function lbVisibleTiles(){
  var all=document.querySelectorAll('[data-lb-url]'),out=[];
  for(var i=0;i<all.length;i++){if(all[i].offsetParent!==null)out.push(all[i]);}
  return out;
}
// The view's "Show more" control, if present and visible: #more-btn for the
// grid/duplicates views, #gallery-more for the paged gallery, #date-more for
// a date's files.
function lbMoreButton(){
  var ids=['more-btn','gallery-more','date-more'];
  for(var i=0;i<ids.length;i++){
    var b=document.getElementById(ids[i]);
    if(b&&b.offsetParent!==null)return b;
  }
  return null;
}
function openTile(el){
  var tiles=lbVisibleTiles();
  lbIndex=tiles.indexOf(el);
  openLb(el.dataset.lbUrl,el.dataset.lbType||'image',el.dataset.lbMeta);
  updateLbNav();
}
function updateLbNav(){
  var tiles=lbVisibleTiles();
  var prev=document.getElementById('lb-prev'),next=document.getElementById('lb-next');
  if(prev)prev.hidden=!(lbIndex>0);
  // Next exists when a later tile is loaded, or a page remains to load.
  if(next)next.hidden=!(lbIndex>=0&&(lbIndex<tiles.length-1||lbMoreButton()));
}
function lbStep(delta){
  if(lbIndex<0)return;
  var tiles=lbVisibleTiles();
  var target=lbIndex+delta;
  if(delta>0&&target>=tiles.length){
    // Past the last loaded tile: pull the next page in, then continue. Only a
    // truly last item (no "Show more" left) stops here.
    var btn=lbMoreButton();
    if(!btn||lbLoading)return;
    var before=tiles.length;
    lbLoading=true;
    btn.click();
    var tries=0;
    (function poll(){
      var t=lbVisibleTiles();
      if(t.length>before){lbLoading=false;openTile(t[before]);return;}
      if(tries++>100){lbLoading=false;return;} // ~5s cap for a slow fetch
      setTimeout(poll,50);
    })();
    return;
  }
  if(target<0||target>=tiles.length)return; // stop at the first item
  openTile(tiles[target]);
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
// The drilled-down pages state their period's size next to the breadcrumb,
// so "is this month worth opening" is answerable before clicking into it.
// Buckets already carry per-child counts; the day gallery reads the files
// response's own total.
function showPeriodCount(n){
  document.getElementById('dateBreadcrumb').insertAdjacentHTML('beforeend',
    ' <span class="date-period-count">'+n+' item'+(n===1?'':'s')+'</span>');
}
// The files loaded into #dateGrid so far, so a mode switch re-renders without a
// refetch. A day, month or range loads routes.date.pageSize files at a time;
// #date-more loads the next page.
var dateFiles=null;
function dateKeepFiles(){ return (typeof KEEPFILES!=='undefined') ? KEEPFILES : []; }

// :warning: The tree comes from /api/dates, not from the rows.
//
// Grouping a page of 200 files by year shows a tree that grows as you scroll,
// which is worse than no tree at all. Counts have to describe the whole library,
// so the server groups and the client draws. One request per level, so a
// response is a few dozen buckets rather than a library.
//
// An inlined page (a static export, which has no server) still groups locally.
function dateInlined(){ return typeof KEEPFILES!=='undefined'; }

function dateHref(prefix){
  return '/date/'+String(prefix).replace(/-/g,'/');
}
function dateCardAttrs(prefix,action){
  if(LIVE_SERVER) return '<a class="date-card-link" href="'+escA(withQuery(dateHref(prefix)))+'" aria-label="Open '+escA(prefix)+'"></a>';
  return '<a class="date-card-link" href="#" onclick="'+action+';return false" aria-label="Open '+escA(prefix)+'"></a>';
}
function dateCrumb(label,prefix,action){
  if(LIVE_SERVER) return '<a href="'+escA(withQuery(dateHref(prefix)))+'">'+escH(label)+'</a>';
  return '<a onclick="'+action+'">'+escH(label)+'</a>';
}
// The topmost breadcrumb, back to the full year overview: the /date route on a
// live server, or buildYearView() in a static export.
function dateRootCrumb(){
  return LIVE_SERVER ? '<a href="'+escA(withQuery('/date'))+'">All Dates</a>' : '<a onclick="buildYearView()">All Dates</a>';
}

function dateCards(buckets,actionFor){
  return buckets.map(function(b){
    var action=actionFor(b.key);
    return '<div class="date-card" data-key="'+escA(b.key)+'">'+
      dateCardAttrs(b.key,action)+
      buildPreview(b.sample)+
      '<div class="date-card-label">'+escH(b.key)+'</div>'+
      '<div class="date-card-count">'+b.count+' files</div></div>';
  }).join('');
}
function fetchBuckets(level,parent,then,target){
  var grid=target||document.getElementById('dateGrid');
  grid.innerHTML='<p class="muted">Loading...</p>';
  var q='/api/dates?level='+encodeURIComponent(level);
  if(parent)q+='&parent='+encodeURIComponent(parent);
  q+=galleryQueryParam();
  fetch(q).then(queryJson)
    .then(function(d){
      if(d.bad){ queryErrorStatus(d); grid.innerHTML=''; return; }
      if(GQUERY&&typeof d.library_total==='number')queryCountStatus(d.matched,d.library_total);
      then(d.buckets||[]);
    })
    .catch(function(){ grid.innerHTML='<p class="muted">Could not load dates.</p>'; });
}
// Groups inlined rows the old way, for a static export.
function groupInlined(len,parent){
  var by={};
  dateKeepFiles().forEach(function(f){
    var d=bestDateJs(f); if(!d)return;
    var k=d.slice(0,len);
    if(parent && d.slice(0,parent.length)!==parent) return;
    (by[k]=by[k]||[]).push(f);
  });
  return Object.keys(by).sort().reverse().map(function(k){
    return {key:k,count:by[k].length,sample:by[k][0]};
  });
}

function buildYearView(){
  resetDatePaging();
  rerunDateView=function(){buildYearView();};
  dateState={level:'year',year:null,month:null};
  document.getElementById('dateBreadcrumb').innerHTML='All Dates';
  var narrowing=document.getElementById('dateNarrowing');
  if(narrowing)narrowing.innerHTML='';
  var draw=function(b){
    if(!b.length&&GQUERY){
      document.getElementById('dateGrid').innerHTML='<p class="muted">No dated files match this query.</p>';
      return;
    }
    if(!b.length&&window.showEmptyState){
      window.showEmptyState('No photos yet',
        '<p>There is nothing to arrange by date: this library has no scanned files.</p>'+
        '<p class="hint">Run <code>videre scan</code> in the library folder to index it, then reload this page.</p>');
      return;
    }
    document.getElementById('dateGrid').innerHTML=
      dateCards(b,function(k){return "buildMonthView('"+k+"')";});
  };
  if(dateInlined())draw(groupInlined(4,null)); else fetchBuckets('year',null,draw);
}
function buildMonthView(year){
  resetDatePaging();
  rerunDateView=function(){buildMonthView(year);};
  dateState={level:'month',year:year,month:null};
  document.getElementById('dateBreadcrumb').innerHTML=
    dateRootCrumb()+' &gt; '+dateCrumb(year,year,'buildYearView()');
  var narrowing=document.getElementById('dateNarrowing');
  if(narrowing)narrowing.innerHTML='';
  var draw=function(b){
    showPeriodCount(b.reduce(function(s,x){return s+x.count;},0));
    document.getElementById('dateGrid').innerHTML=
      dateCards(b,function(k){return "buildDayView('"+k+"')";});
  };
  if(dateInlined())draw(groupInlined(7,year)); else fetchBuckets('month',year,draw);
}
function buildDayView(month){
  resetDatePaging();
  rerunDateView=function(){buildDayView(month);};
  dateState={level:'day',year:dateState.year||month.slice(0,4),month:month};
  document.getElementById('dateBreadcrumb').innerHTML=
    dateRootCrumb()+' &gt; '+
    dateCrumb(dateState.year,dateState.year,'buildYearView()')+' &gt; '+
    dateCrumb(month,month,"buildMonthView('"+dateState.year+"')");
  var narrowing=document.getElementById('dateNarrowing');
  if(narrowing)narrowing.innerHTML='';
  var draw=function(b){
    showPeriodCount(b.reduce(function(s,x){return s+x.count;},0));
    document.getElementById('dateGrid').innerHTML=
      dateCards(b,function(k){return "buildDayGallery('"+k+"')";});
  };
  if(dateInlined())draw(groupInlined(10,month)); else fetchBuckets('day',month,draw);
}
function buildDayGallery(day){
  resetDatePaging();
  rerunDateView=function(){buildDayGallery(day);};
  document.getElementById('dateBreadcrumb').innerHTML=
    dateRootCrumb()+' &gt; '+
    dateCrumb(dateState.year,dateState.year,'buildYearView()')+' &gt; '+
    dateCrumb(dateState.month,dateState.month,"buildMonthView('"+dateState.year+"')")+' &gt; '+escH(day);
  var narrowing=document.getElementById('dateNarrowing');
  if(narrowing)narrowing.innerHTML='';
  var grid=document.getElementById('dateGrid');
  if(dateInlined()){
    var files=dateKeepFiles().filter(function(f){
      var d=bestDateJs(f); return d && d.slice(0,10)===day;
    });
    showPeriodCount(files.length);
    renderDateFiles(sortFiles(files),'No files for '+day+'.');
    return;
  }
  fetchDateFiles('date='+encodeURIComponent(day),'No files for '+day+'.');
}
// A large day (a wedding, a trip, a Takeout import) can hold thousands of
// files, so a date loads a page at a time like the Library does, and the
// period count is the server's total, not what is loaded.
var DPAGE=settingIntInRange('routes.date.pageSize',1,500);
var dateParams=null,dateEmptyText='',dateTotal=0,dateRequest=0,dateLoading=false;
// Every date view starts here: a reply still in flight for the previous view is
// dropped, and the button belongs to no view until a date's files load.
function resetDatePaging(){
  dateRequest++;
  dateParams=null;
  dateFiles=null;
  dateTotal=0;
  dateLoading=false;
  updateDateMore();
}
function fetchDateFiles(params,emptyText){
  resetDatePaging();
  dateParams=params;
  dateEmptyText=emptyText;
  dateFiles=[];
  document.getElementById('dateGrid').innerHTML='<p class="muted">Loading...</p>';
  loadDatePage(dateRequest);
}
function moreDateFiles(){
  if(dateParams!==null&&!dateLoading)loadDatePage(dateRequest);
}
function loadDatePage(request){
  var first=!dateFiles.length;
  var btn=document.getElementById('date-more');
  dateLoading=true;
  if(!first&&btn)btn.textContent='Loading\u2026';
  fetch('/api/files?view=date&'+dateParams+'&offset='+dateFiles.length+'&limit='+DPAGE+
        sortQuery()+galleryQueryParam())
    .then(queryJson)
    .then(function(d){
      if(request!==dateRequest)return;
      dateLoading=false;
      var grid=document.getElementById('dateGrid');
      if(d.bad){ queryErrorStatus(d); grid.innerHTML=''; dateParams=null; updateDateMore(); return; }
      var files=dateFiles.concat(d.files||[]);
      dateTotal=d.total!=null?d.total:files.length;
      if(first)showPeriodCount(dateTotal);
      renderDateFiles(files,dateEmptyText);
      updateDateMore();
    })
    .catch(function(){
      if(request!==dateRequest)return;
      dateLoading=false;
      if(first){ document.getElementById('dateGrid').innerHTML='<p class="muted">Could not load that date.</p>'; return; }
      // Say so, rather than leaving a button that silently does nothing.
      if(btn)btn.textContent='Could not load more. Click to retry.';
    });
}
function updateDateMore(){
  var btn=document.getElementById('date-more');
  if(!btn)return;
  var rem=dateParams!==null&&dateFiles?dateTotal-dateFiles.length:0;
  if(rem>0){btn.style.display='inline-block';btn.textContent='Show more ('+rem+' remaining)';}
  else btn.style.display='none';
}
function renderDateBreadcrumb(prefix){
  var parts=prefix.split('-');
  var root=dateRootCrumb()+' &gt; ';
  if(parts.length===1){
    document.getElementById('dateBreadcrumb').innerHTML=root+dateCrumb(parts[0],parts[0],'buildYearView()');
  }else if(parts.length===2){
    document.getElementById('dateBreadcrumb').innerHTML=root+
      dateCrumb(parts[0],parts[0],'buildYearView()')+' &gt; '+
      dateCrumb(prefix,prefix,"buildMonthView('"+parts[0]+"')");
  }else{
    var month=parts[0]+'-'+parts[1];
    document.getElementById('dateBreadcrumb').innerHTML=root+
      dateCrumb(parts[0],parts[0],'buildYearView()')+' &gt; '+
      dateCrumb(month,month,"buildMonthView('"+parts[0]+"')")+' &gt; '+escH(prefix);
  }
}
function renderDateNarrowing(prefix){
  var narrowing=document.getElementById('dateNarrowing');
  if(!narrowing)return;
  narrowing.innerHTML='';
  var level=null;
  if(prefix.length===4)level='month';
  if(prefix.length===7)level='day';
  if(!level)return;
  fetchBuckets(level,prefix,function(b){
    narrowing.innerHTML=dateCards(b,function(k){
      return level==='month' ? "buildDayView('"+k+"')" : "buildDayGallery('"+k+"')";
    });
  },narrowing);
}
function buildPrefixGallery(prefix){
  resetDatePaging();
  rerunDateView=function(){buildPrefixGallery(prefix);};
  var parts=prefix.split('-');
  dateState.year=parts[0]||null;
  dateState.month=parts.length>=2 ? parts[0]+'-'+parts[1] : null;
  renderDateBreadcrumb(prefix);
  renderDateNarrowing(prefix);
  fetchDateFiles('date='+encodeURIComponent(prefix),'No files for '+prefix+'.');
}
function buildRangeGallery(range){
  resetDatePaging();
  rerunDateView=function(){buildRangeGallery(range);};
  document.getElementById('dateBreadcrumb').innerHTML=dateRootCrumb()+' &gt; Date range';
  var narrowing=document.getElementById('dateNarrowing');
  if(narrowing)narrowing.innerHTML='';
  var params=[];
  if(range.from)params.push('from='+encodeURIComponent(range.from));
  if(range.to)params.push('to='+encodeURIComponent(range.to));
  fetchDateFiles(params.join('&'),'No files for this range.');
}
function buildDateInitialView(){
  if(typeof GDATE==='object'&&GDATE&&GDATE.kind==='prefix')buildPrefixGallery(GDATE.value);
  else if(typeof GDATE==='object'&&GDATE&&GDATE.kind==='range')buildRangeGallery(GDATE);
  else buildYearView();
}

// Events reuse the date view's grid, breadcrumb and card styles. The server
// infers exact travel trips; the client only draws them.
function eventDateRange(from,to){
  // from/to are "YYYY-MM-DD HH:MM:SS"; show a compact, human span.
  var months=['Jan','Feb','Mar','Apr','May','Jun','Jul','Aug','Sep','Oct','Nov','Dec'];
  var f=from.slice(0,10).split('-'), t=to.slice(0,10).split('-');
  var fy=f[0], fm=+f[1], fd=+f[2], ty=t[0], tm=+t[1], td=+t[2];
  if(from.slice(0,10)===to.slice(0,10)) return months[fm-1]+' '+fd+', '+fy;
  if(fy===ty&&fm===tm) return months[fm-1]+' '+fd+'–'+td+', '+fy;
  if(fy===ty) return months[fm-1]+' '+fd+' – '+months[tm-1]+' '+td+', '+fy;
  return months[fm-1]+' '+fd+', '+fy+' – '+months[tm-1]+' '+td+', '+ty;
}
function eventCards(events){
  return events.map(function(e){
    var range=eventDateRange(e.start,e.end);
    var label=e.title||range;
    var sub=escH(range)+' · '+e.count+' file'+(e.count===1?'':'s');
    return '<div class="date-card event-card" data-key="'+escA(e.key)+'">'+
      '<a class="date-card-link" href="'+escA(withQuery('/events/'+e.key))+'" aria-label="Open '+escA(label)+'"></a>'+
      buildPreview(e.sample)+
      '<div class="date-card-label">'+escH(label)+'</div>'+
      '<div class="date-card-count">'+sub+'</div></div>';
  }).join('');
}
// Event start/end are "YYYY-MM-DD HH:MM:SS" local wall-clock, so the string
// order is the time order and a span needs only the difference.
function eventMillis(t){ return Date.parse(t.replace(' ','T'))||0; }
function sortEvents(evs){
  var s=sortState(),asc=(s.dir==='asc');
  function key(e){
    if(s.field==='files')return e.count;
    if(s.field==='length')return eventMillis(e.end)-eventMillis(e.start);
    if(s.field==='name')return (e.title||eventDateRange(e.start,e.end)).toLowerCase();
    return e.start;
  }
  return evs.slice().sort(function(a,b){
    var ka=key(a),kb=key(b);
    if(ka<kb)return asc?-1:1;
    if(ka>kb)return asc?1:-1;
    return a.start<b.start?1:(a.start>b.start?-1:0);
  });
}
var eventsResponse=null; // the overview's /api/events answer, so a sort change does not recompute trips
function buildEventsOverview(){
  rerunDateView=function(){buildEventsOverview();};
  document.getElementById('dateBreadcrumb').innerHTML='Events';
  var narrowing=document.getElementById('dateNarrowing');
  if(narrowing)narrowing.innerHTML='';
  var grid=document.getElementById('dateGrid');
  if(!eventsResponse)grid.innerHTML='<p class="muted">Loading…</p>';
  (eventsResponse?Promise.resolve(eventsResponse):fetch('/api/events'+(GQUERY?'?q='+encodeURIComponent(GQUERY):'')).then(queryJson))
    .then(function(d){
      if(d.bad){ queryErrorStatus(d); grid.innerHTML=''; return; }
      eventsResponse=d;
      if(GQUERY&&typeof d.library_total==='number')queryCountStatus(d.matched,d.library_total);
      if(d.empty_reason==='no_matching_events'){
        grid.innerHTML='<p class="muted">No trip has a matching file.</p>';
        return;
      }
      var evs=sortEvents(d.events||[]);
      var reasons={
        no_media:'No media has been scanned yet.',
        no_capture_dates:'Events needs photo capture dates or video creation dates.',
        insufficient_location_evidence:'Events needs dated, location-supported photos to recognize travel.',
        no_qualifying_trips:'No substantial travel trips were found yet.'
      };
      var why='<p>'+escH(reasons[d.empty_reason]||reasons.no_qualifying_trips)+
        '</p><p class="hint">Events finds travel from dated, location-supported photos.</p>';
      if(!evs.length){ window.showEmptyState('No travel trips yet',why); return; }
      grid.innerHTML=eventCards(evs);
    })
    .catch(function(){ grid.innerHTML='<p class="muted">Could not load events.</p>'; });
}
function buildEventLeaf(ev){
  rerunDateView=function(){buildEventLeaf(ev);};
  var range=eventDateRange(ev.from,ev.to);
  document.getElementById('dateBreadcrumb').innerHTML=
    '<a href="'+escA(withQuery('/events'))+'">All Events</a> &gt; '+escH(ev.title||range);
  var narrowing=document.getElementById('dateNarrowing');
  if(narrowing)narrowing.innerHTML='';
  var grid=document.getElementById('dateGrid');
  grid.innerHTML='<p class="muted">Loading…</p>';
  fetch('/api/events/'+encodeURIComponent(ev.key)+'/files'+(GQUERY?'?q='+encodeURIComponent(GQUERY):''))
    .then(queryJson)
    .then(function(d){
      if(d.bad){ queryErrorStatus(d); grid.innerHTML=''; return; }
      showPeriodCount(d.total!=null?d.total:(d.files||[]).length);
      renderDateFiles(sortFiles(d.files||[]),'No files in this event.');
    })
    .catch(function(){ grid.innerHTML='<p class="muted">Could not load this event.</p>'; });
}
function buildEventsInitialView(){
  if(typeof GEVENT==='object'&&GEVENT) buildEventLeaf(GEVENT);
  else buildEventsOverview();
}
// Event delegation: toggle, lightbox, copy. One listener for all dynamic content
document.addEventListener('click',function(e){
  if(selectModeClick(e))return;
  var lb=e.target.closest('[data-lb-url]');
  if(lb){e.preventDefault();e.stopPropagation();openTile(lb);return;}
  var cp=e.target.closest('[data-path]');
  if(cp){copyPath(cp.dataset.path);return;}
  var hdr=e.target.closest('.group-header');
  if(hdr){toggle(hdr.closest('.group').id);return;}
});
document.addEventListener('keydown',function(e){
  if(e.key==='Escape'&&selectMode&&!document.getElementById('lb').classList.contains('on')){
    if(fileSelection)fileSelection.clear();
    return;
  }
  if(e.key==='Escape'){closeLb();return;}
  if(!document.getElementById('lb').classList.contains('on'))return;
  if(e.key==='ArrowLeft'){e.preventDefault();lbStep(-1);}
  else if(e.key==='ArrowRight'){e.preventDefault();lbStep(1);}
});
document.getElementById('lb').addEventListener('click',function(e){
  if(e.target===this)closeLb();
});
// The lightbox stage keeps its clicks to itself, so its metadata panel
// listens on its own: the copy button beside the file name.
(function(){
  var meta=document.getElementById('lbMeta');
  if(!meta)return;
  meta.addEventListener('click',function(e){
    var ct=e.target.closest('[data-copy-text]');
    if(!ct)return;
    copyPath(ct.dataset.copyText);
    ct.innerHTML=ICON_CHECK;
    ct.title='Copied';
    setTimeout(function(){ ct.innerHTML=ICON_COPY; ct.title='Copy file name'; },1200);
  });
})();

// Click to zoom (toggle 1:1), drag to pan, wheel to pan while zoomed. A click
// is distinguished from a pan by the pointer barely moving, so dragging the
// zoomed image never toggles it back to fit.
(function(){
  var img=document.getElementById('lb-img');
  if(!img)return;
  var down=false,moved=false,sx=0,sy=0,ox=0,oy=0,pid=null;
  img.addEventListener('wheel',function(e){
    if(img.style.display==='none')return;
    // The wheel no longer zooms: zoom is reached only by clicking the image
    // (toggle 1:1) or the fullscreen button. While zoomed the wheel pans over
    // the enlarged image; while fit-to-screen it does nothing, but is still
    // swallowed so the page behind the open lightbox does not scroll.
    e.preventDefault();
    if(lbz.scale<=1)return;
    lbz.x-=e.deltaX; lbz.y-=e.deltaY;
    lbClampPan(); lbApply();
  },{passive:false});
  img.addEventListener('pointerdown',function(e){
    if(img.style.display==='none')return;
    down=true;moved=false;sx=e.clientX;sy=e.clientY;ox=lbz.x;oy=lbz.y;pid=e.pointerId;
    try{img.setPointerCapture(pid);}catch(err){}
  });
  img.addEventListener('pointermove',function(e){
    if(!down)return;
    var dx=e.clientX-sx,dy=e.clientY-sy;
    if(Math.abs(dx)>4||Math.abs(dy)>4)moved=true;
    if(lbz.scale>1){ lbz.x=ox+dx; lbz.y=oy+dy; lbClampPan(); lbApply(); }
  });
  function end(e){
    if(!down)return;
    down=false;
    try{img.releasePointerCapture(pid);}catch(err){}
    if(!moved)lbToggleZoom(e.clientX,e.clientY);
  }
  img.addEventListener('pointerup',end);
  img.addEventListener('pointercancel',function(){down=false;});
})();

// ---- All-files gallery and similarity search (active only with --all) ----
// RESULT_ROWS holds only the rows a search returned, a couple of dozen at most.
// HASH_FILES stays for the inlined static export, whose rows carry no `copies`
// field and so must be counted client-side.
// GPAGE is capped at 500, the most /api/files returns per request.
var GPAGE=settingIntInRange('routes.files.pageSize',1,500),gShown=0,HASH_FILES={},RESULT_ROWS={},galleryFiles=[];
var GLOCATION=null,gRequest=0;
// The nav box's query, on the pages that honour one (templates/query-box.js):
// its filters narrow what the page shows, and on the Files page its words, if
// any, rank in the results strip.
var GQUERY=((window.videreQueryHonours?window.videreQueryHonours(location.pathname):location.pathname==='/')
  ?(new URLSearchParams(location.search).get('q')||''):'');
var gQueryRanked=false;
function galleryQueryParam(){
  return GQUERY?'&q='+encodeURIComponent(GQUERY):'';
}
// A link within the page's own route keeps the query.
function withQuery(href){
  return GQUERY?href+(href.indexOf('?')<0?'?':'&')+'q='+encodeURIComponent(GQUERY):href;
}
// This page without the query, for "clear".
function withoutQuery(){
  var p=new URLSearchParams(location.search);
  p.delete('q');
  var rest=p.toString();
  return location.pathname+(rest?'?'+rest:'');
}
function queryCountStatus(matched,total){
  showQueryStatus('<strong>'+(matched||0).toLocaleString()+'</strong> of '+
    total.toLocaleString()+' match <code>'+escH(GQUERY)+'</code>'+
    ' &middot; <a href="'+escA(withoutQuery())+'">clear</a>',false);
}
// A 400 from a route that took the query: say why, and where, rather than
// show unfiltered results that look like an answer.
function queryErrorStatus(d){
  var at=(typeof d.at==='number')?' (at character '+(d.at+1)+')':'';
  showQueryStatus('Cannot run <code>'+escH(GQUERY)+'</code>: '+escH(d.error||'')+at+
    ' &middot; <a href="https://docs.videre.sh/reference/query-syntax/" target="_blank" rel="noopener">syntax</a>',true);
}
// A page rendered with its query applied (Duplicates) carries the outcome.
if(GQUERY&&typeof GQUERY_RESULT==='object'&&GQUERY_RESULT){
  document.addEventListener('DOMContentLoaded',function(){
    if(GQUERY_RESULT.error!=null)queryErrorStatus(GQUERY_RESULT);
    else queryCountStatus(GQUERY_RESULT.matched,GQUERY_RESULT.library_total);
  });
}
// A route's JSON, with a 400 marked `bad` rather than thrown.
function queryJson(r){
  if(r.status===400)return r.json().then(function(e){ e.bad=true; return e; });
  return r.json();
}
// "N of M", or why the query could not run, in a line above the grid.
function showQueryStatus(html,isError){
  var g=(location.pathname==='/duplicates'&&document.getElementById('groups-container'))||
    document.getElementById('gallery')||document.getElementById('dateBreadcrumb');
  if(!g)return;
  var s=document.getElementById('query-status');
  if(!s){
    s=document.createElement('div');
    s.id='query-status';
    s.className='query-status';
    g.parentNode.insertBefore(s,g);
  }
  s.classList.toggle('query-error',!!isError);
  s.innerHTML=html;
}
function galleryLocationQuery(){
  if(!GLOCATION)return '';
  return '&lat='+encodeURIComponent(GLOCATION.lat)+
    '&lon='+encodeURIComponent(GLOCATION.lon)+
    '&radius='+encodeURIComponent(GLOCATION.radius);
}
// See faces.js: the labeling sub-pages are not always under /people.
function peopleRootG(){
  var r=(typeof PEOPLE_ROOT==='string')?PEOPLE_ROOT:'/people';
  return r.charAt(r.length-1)==='/'?r:r+'/';
}
function bestDateJs(f){
  // The server's resolved capture date: EXIF, a video's own date, a Takeout
  // sidecar, or the file's time, already on the local clock.
  if(f.ca)return f.ca;
  if(f.ex&&f.ex.indexOf('0000')!==0)return f.ex;
  if(f.cr&&f.mo)return f.cr<f.mo?f.cr:f.mo;
  return f.cr||f.mo||'';
}
// A stored date is wall-clock text ("YYYY-MM-DDTHH:MM:SS", no timezone); format
// it from the parts directly so no timezone conversion shifts it, falling back
// to the raw string if it is not the expected shape.
function humanDate(s){
  if(!s)return '';
  var m=/^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2}):(\d{2}))?/.exec(s);
  if(!m)return s;
  var mon=['Jan','Feb','Mar','Apr','May','Jun','Jul','Aug','Sep','Oct','Nov','Dec'];
  var out=(+m[3])+' '+mon[+m[2]-1]+' '+m[1];
  if(m[4])out+=', '+m[4]+':'+m[5];
  return out;
}
// Similarity is a server feature now, so it works at every library size. It
// used to appear only when the page had downloaded every vector, which switched
// it off for exactly the libraries big enough to want it. A static export has
// no server to ask, so it has no button.
function similarBtn(hash){
  if(typeof LIVE_SERVER==='undefined'||!LIVE_SERVER)return '';
  // The Similar search ranks against embeddings; with none, it can only fail,
  // so offer no button rather than one that returns "Search failed".
  if(typeof HAS_EMBEDDINGS!=='undefined'&&!HAS_EMBEDDINGS)return '';
  // Results draw into #results (Files and Map); a page without it would
  // throw on the answer, so it offers no button.
  if(!document.getElementById('results'))return '';
  return '<button class="similar-btn" data-similar="'+escA(hash)+'">Similar</button>';
}
// ---- Tile view (justified rows) --------------------------------------------
// An image-first layout for the Files and Date galleries, beside List. The
// default and the last choice come from the library's gallery settings
// (`routes.files.view`). Tile reuses buildPreview for the tile body, so HEIC
// and video handling and the decode-failure gate are unchanged, and positions
// each tile with the vendored justified-layout over the items' aspect ratios.
// No caption.
function viewMode(){ return settingOneOf('routes.files.view',['list','tile']); }
function storeViewMode(m){ saveSetting('routes.files.view',m); }
// Older scans carry no w/h, so fall back to square rather than dropping the item.
function tileRatio(f){ return (f.w&&f.h) ? (f.w/f.h) : 1; }
// One tile: the existing preview markup, positioned absolutely from the box the
// layout computed. buildPreview already carries data-lb-url/-type/-meta, so the
// lightbox and its nav keep working and DOM order stays file (row) order.
function tileHtml(f,box){
  return '<div class="tile" data-hash="'+escA(f.hash)+'" style="left:'+box.left+'px;top:'+box.top+'px;'+
    'width:'+box.width+'px;height:'+box.height+'px">'+buildPreview(f)+likedBadge(f)+'</div>';
}
// A liked item's red heart, bottom left over its thumbnail. Display only: it
// takes no clicks (the thumbnail under it still opens the lightbox), and the
// lightbox heart is the one that changes it.
function likedBadge(f){
  return f.liked?'<span class="liked-badge" title="Liked" aria-label="Liked">'+ICON_HEART+'</span>':'';
}
// Where an item's badge sits: over a tile, or over a list card's preview.
function likedBadgeHost(el){
  return el.classList.contains('card')?el.querySelector('.card-preview'):el;
}
// Lay files out as justified rows inside container. Pure geometry over ratios,
// so re-running it on append or resize is cheap.
function layoutTiles(container,files){
  var width=container.clientWidth||container.offsetWidth||0;
  if(!width){ requestAnimationFrame(function(){layoutTiles(container,files);}); return; }
  // Match the list view's gutters (.gallery padding: 12px 16px): the layout
  // offsets every box by this padding and folds top+bottom into the container
  // height, so tile mode lines up with the list grid and the strip above it.
  var geo=justifiedLayout(files.map(tileRatio),{
    containerWidth:width, containerPadding:{top:12,right:16,bottom:12,left:16},
    boxSpacing:{horizontal:settingInRange('routes.files.tile.colGap',0,100),
                vertical:settingInRange('routes.files.tile.rowGap',0,100)},
    targetRowHeight:settingInRange('routes.files.tile.rowHeight',80,1000)
  });
  var html='';
  for(var i=0;i<files.length;i++) html+=tileHtml(files[i],geo.boxes[i]);
  container.classList.add('tile-mode');
  container.style.height=geo.containerHeight+'px';
  container.innerHTML=html;
}
// Leave tile mode: drop the absolute-positioning class and inline height so the
// list renderer's normal flow returns.
function clearTileMode(container){
  container.classList.remove('tile-mode');
  container.style.height='';
}
// Re-render whichever gallery is on this page in the current mode, from data
// already loaded (no refetch). The Date grid branch is added with the Date tab.
function renderCurrentMode(){
  var g=document.getElementById('gallery');
  if(g){
    if(viewMode()==='tile'){ layoutTiles(g,galleryFiles); }
    else { clearTileMode(g); g.innerHTML=galleryFiles.map(buildCard).join(''); }
  }
  var grid=document.getElementById('dateGrid');
  if(grid && dateFiles){ renderDateFiles(dateFiles); }
}
// The one place the Date galleries render their files, so List and Tile modes
// and a later mode switch share a path. It re-renders every loaded file, so a
// page appended by Show more lays out with the ones before it.
function renderDateFiles(files,emptyText){
  dateFiles=files;
  var grid=document.getElementById('dateGrid');
  if(viewMode()==='tile'){
    layoutTiles(grid,files);
  } else {
    clearTileMode(grid);
    grid.innerHTML=files.length
      ? files.map(function(f){return buildCard(f);}).join('')
      : '<p class="muted">'+escH(emptyText||'No files for this date.')+'</p>';
  }
}
function setViewMode(m){
  storeViewMode(m);
  document.querySelectorAll('.view-mode-select').forEach(function(s){ s.value=m; });
  renderCurrentMode();
}
function buildCard(f){
  var fname=f.path.split('/').pop()||f.path;
  // `copies` arrives on the row from /api/files. An inlined page has no such
  // field and falls back to counting the array, which is the only reason that
  // array ever had to be complete.
  var n = (typeof f.copies==='number') ? f.copies
        : (HASH_FILES[f.hash] ? HASH_FILES[f.hash].length : 1);
  var copies = n>1 ? '<span class="copies">x'+n+'</span>' : '';
  // A ranked card (a search's own page) shows its score.
  var score = (typeof f._score==='number') ? '<span class="score">'+f._score.toFixed(3)+'</span>' : '';
  return '<div class="card" data-hash="'+escA(f.hash)+'">'+score+copies+
    '<span class="card-preview">'+buildPreview(f)+likedBadge(f)+'</span>'+
    '<div class="card-meta" title="'+escA(f.path)+'">'+escH(fname)+'</div>'+
    '<div class="card-meta">'+fmtB(f.size)+(bestDateJs(f)?' &middot; '+escH(bestDateJs(f)):'')+'</div>'+
    similarBtn(f.hash)+
    '</div>';
}
// Appends one page of cards and updates the button. Shared by both paths, so an
// inlined page and a fetched one render identically.
function appendCards(files,total){
  for(var i=0;i<files.length;i++)galleryFiles.push(files[i]);
  gShown+=files.length;
  var g=document.getElementById('gallery');
  if(viewMode()==='tile'){
    // Re-run the layout over all loaded items: pure geometry over ratios, cheap.
    layoutTiles(g,galleryFiles);
  } else {
    clearTileMode(g);
    var html='';
    for(var j=0;j<files.length;j++)html+=buildCard(files[j]);
    var tmp=document.createElement('div');
    tmp.innerHTML=html;
    while(tmp.firstChild)g.appendChild(tmp.firstChild);
  }
  var btn=document.getElementById('gallery-more');
  if(!btn)return;
  var rem=total-gShown;
  if(rem>0){btn.style.display='inline-block';btn.textContent='Show more ('+rem+' remaining)';}
  else btn.style.display='none';
}
// Guards a second fetch while one is in flight; a double-click on "Show more"
// would otherwise append the same page twice.
var gLoading=false;
function renderGallery(){
  if(typeof ALLFILES!=='undefined'){
    if(allFilesSorted===null)allFilesSorted=sortFiles(ALLFILES.slice());
    appendCards(allFilesSorted.slice(gShown,gShown+GPAGE),allFilesSorted.length);
    return;
  }
  if(gLoading)return;
  gLoading=true;
  var request=++gRequest;
  var btn=document.getElementById('gallery-more');
  if(btn)btn.textContent='Loading\u2026';
  fetch('/api/files?view='+encodeURIComponent(GVIEW)+'&offset='+gShown+'&limit='+GPAGE+
        galleryLocationQuery()+sortQuery()+galleryQueryParam())
    .then(queryJson)
    .then(function(d){
      if(request!==gRequest)return;
      gLoading=false;
      if(d.bad){
        queryErrorStatus(d);
        if(btn)btn.style.display='none';
        return;
      }
      if(GQUERY&&typeof d.library_total==='number'&&!gShown){
        queryCountStatus(d.total,d.library_total);
      }
      // The query's words rank, once, in the results strip, within its filters.
      if(GQUERY&&d.text&&!gQueryRanked){
        gQueryRanked=true;
        runTextSearch(GQUERY);
      }
      if(!d.total&&!gShown&&location.pathname==='/'&&!GQUERY){
        window.showEmptyState('No photos yet',
          '<p>This library has no scanned files.</p>'+
          '<p class="hint">Run <code>videre scan</code> in the library folder to index it, then reload this page.</p>');
        return;
      }
      appendCards(d.files||[],d.total||0);
    })
    .catch(function(){
      if(request!==gRequest)return;
      gLoading=false;
      // Say so, rather than leaving a button that silently does nothing.
      if(btn)btn.textContent='Could not load more. Click to retry.';
    });
}
// The Search page pages its ranking, not the library: while it has more, its
// next page is what Show more loads. Cleared during a load, so a double click
// cannot append one page twice.
var SPAGE=settingIntInRange('routes.search.pageSize',1,200),searchMore=null;
function showMoreGallery(){
  if(searchMore){ var next=searchMore; searchMore=null; next(); return; }
  renderGallery();
}
// The map page owns the location selection, while the shared gallery owns
// paging and rendering. Reset all paging state before loading the first page
// for the selected location; null returns to the complete library.
window.setGalleryLocation=function(lat,lon,radius){
  GLOCATION=(lat===null||lon===null||radius===null)?null:{lat:lat,lon:lon,radius:radius};
  gRequest++;
  gLoading=false;
  gShown=0;
  galleryFiles=[];
  var gallery=document.getElementById('gallery');
  if(gallery){clearTileMode(gallery);gallery.innerHTML='';}
  renderGallery();
};
function findSimilar(hash){
  var panel=document.getElementById('results');
  if(panel){panel.style.display='block';panel.innerHTML='<div class="results-head"><h2>Searching&hellip;</h2></div>';}
  fetch('/api/search?like='+encodeURIComponent(hash)+'&limit=24')
    .then(function(r){ if(!r.ok)throw 0; return r.json(); })
    .then(function(d){ renderResults(hash,d.results||[]); })
    .catch(function(){
      if(panel)panel.innerHTML='<div class="results-head"><h2>Search failed</h2>'+
        '<button onclick="clearResults()">Close</button></div>';
    });
}
function resultCard(hash,score,isQuery){
  var f=RESULT_ROWS[hash];
  if(!f)return '';
  var fname=f.path.split('/').pop()||f.path;
  var badge=isQuery?'':'<span class="score">'+score.toFixed(3)+'</span>';
  var n=(typeof f.copies==='number')?f.copies:1;
  var copies=n>1?'<span class="copies">x'+n+'</span>':'';
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
  for(var i=0;i<scored.length;i++)need.push(scored[i].hash);
  var missing=need.filter(function(h){return !RESULT_ROWS[h];});
  if(missing.length===0){ drawResults(qHash,scored); return; }
  fetch('/api/files?hashes='+encodeURIComponent(missing.join(',')))
    .then(function(r){ return r.json(); })
    .then(function(d){
      (d.files||[]).forEach(function(f){ RESULT_ROWS[f.hash]=f; });
      drawResults(qHash,scored);
    })
    .catch(function(){ drawResults(qHash,scored); });
}
function drawResults(qHash,scored){
  var panel=document.getElementById('results');
  var html='<div class="results-head"><h2>Similar images</h2>'+
    searchPageLink('like='+encodeURIComponent(qHash)+filtersParam())+
    '<button onclick="clearResults()">Close</button></div>'+
    '<div class="results-strip">'+resultCard(qHash,1,true);
  for(var i=0;i<scored.length;i++){
    html+=resultCard(scored[i].hash,scored[i].score,false);
  }
  html+='</div>';
  panel.innerHTML=html;
  panel.style.display='block';
  panel.querySelectorAll('img').forEach(function(img){if(img.loading==='lazy')img.loading='eager';});
  panel.scrollIntoView({behavior:'smooth',block:'start'});
}
// A strip's results as a page of their own, which a link or bookmark reopens.
// Live only: a static export has no server to search.
function searchPageLink(params){
  if(typeof LIVE_SERVER==='undefined'||!LIVE_SERVER)return '';
  return '<a class="results-open" href="/search?'+escA(params)+'">Open as a page</a>';
}
function filtersParam(){
  var f=window.videreQueryFilters?window.videreQueryFilters():'';
  return f?'&q='+encodeURIComponent(f):'';
}
// /search: the Library page ranking instead of listing. `like` ranks by an
// example, the query's words by meaning; its filters narrow what is ranked.
// Filters alone rank nothing, so they list, as on Library.
function runSearchPage(){
  var params=new URLSearchParams(location.search);
  var like=params.get('like')||'';
  var nav=document.getElementById('nav-search');
  var words=nav?nav.value.trim():'';
  if(!like&&!words){ renderGallery(); return; }
  var g=document.getElementById('gallery');
  var head=document.createElement('div');
  head.id='search-head';
  head.className='results-head';
  head.innerHTML='<h2>Searching&hellip;</h2>';
  g.parentNode.insertBefore(head,g);
  var btn=document.getElementById('gallery-more');
  if(btn)btn.style.display='none';
  var base='/api/search?limit='+SPAGE+(like?'&like='+encodeURIComponent(like):'')+galleryQueryParam();
  // Where the next page starts in the ranking. Kept apart from the cards
  // shown, since a ranked hash with no row is skipped but still ranked.
  var offset=0,title=null;
  function page(){
    if(btn)btn.textContent='Loading…';
    return fetch(base+'&offset='+offset).then(queryJson).then(function(d){
      if(d.bad){ queryErrorStatus(d); head.innerHTML=''; return; }
      var scored=d.results||[];
      offset+=scored.length;
      var hashes=scored.map(function(s){return s.hash;});
      if(like&&title===null)hashes.push(like);
      var rows=hashes.length?fetch('/api/files?hashes='+encodeURIComponent(hashes.join(','))).then(function(r){return r.json();})
                            :Promise.resolve({files:[]});
      return rows.then(function(rd){
        var by={};
        (rd.files||[]).forEach(function(f){ if(!by[f.hash])by[f.hash]=f; });
        if(title===null)title=like
          ?'Similar to '+escH(by[like]?(by[like].path.split('/').pop()||by[like].path):like.slice(0,12))
          :'Results for &ldquo;'+escH(words)+'&rdquo;';
        var files=scored.map(function(s){
          var f=by[s.hash]; if(!f)return null;
          var copy={}; for(var k in f)copy[k]=f[k];
          copy._score=s.score;
          return copy;
        }).filter(Boolean);
        var shown=gShown+files.length;
        head.innerHTML='<h2>'+title+'</h2><span class="results-count">'+shown+' result'+(shown===1?'':'s')+'</span>';
        if(!shown){ g.innerHTML='<p class="muted">Nothing ranked.</p>'; return; }
        appendCards(files,shown);
        searchMore=d.more?next:null;
        if(btn&&d.more){ btn.style.display='inline-block'; btn.textContent='Show more'; }
      });
    });
  }
  function next(){
    page().catch(function(){
      if(title===null){ head.innerHTML='<h2>Search failed</h2>'; return; }
      // Say so, rather than leaving a button that silently does nothing.
      searchMore=next;
      if(btn)btn.textContent='Could not load more. Click to retry.';
    });
  }
  next();
}
function clearResults(){
  var panel=document.getElementById('results');
  panel.style.display='none';
  panel.innerHTML='';
}
// The nav search box submits to `/?q=`; the Files page reads it here and ranks
// the library semantically through the same /api/search endpoint the Similar
// button uses, rendering into the same #results strip. Only the Files page has
// that panel, so a search from another section navigates here first.
function runTextSearch(q){
  var panel=document.getElementById('results');
  if(!panel)return;
  panel.style.display='block';
  panel.innerHTML='<div class="results-head"><h2>Searching&hellip;</h2></div>';
  fetch('/api/search?q='+encodeURIComponent(q)+'&limit=48')
    .then(function(r){ if(!r.ok)throw 0; return r.json(); })
    .then(function(d){ resolveTextResults(q,d.results||[]); })
    .catch(function(){
      panel.innerHTML='<div class="results-head"><h2>Search failed</h2>'+
        '<button onclick="clearResults()">Close</button></div>';
    });
}
// Search returns a ranking of hashes; resolve the rows behind them by hash
// (the same seam similarity uses) before drawing. See renderResults.
function resolveTextResults(query,scored){
  var missing=scored.map(function(s){return s.hash;}).filter(function(h){return !RESULT_ROWS[h];});
  if(missing.length===0){ drawTextResults(query,scored); return; }
  fetch('/api/files?hashes='+encodeURIComponent(missing.join(',')))
    .then(function(r){ return r.json(); })
    .then(function(d){
      (d.files||[]).forEach(function(f){ RESULT_ROWS[f.hash]=f; });
      drawTextResults(query,scored);
    })
    .catch(function(){ drawTextResults(query,scored); });
}
function drawTextResults(query,scored){
  var panel=document.getElementById('results');
  var html='<div class="results-head"><h2>Results for &ldquo;'+escH(query)+'&rdquo;</h2>'+
    searchPageLink('q='+encodeURIComponent(query))+
    '<button onclick="clearResults()">Close</button></div><div class="results-strip">';
  for(var i=0;i<scored.length;i++)html+=resultCard(scored[i].hash,scored[i].score,false);
  html+='</div>';
  if(!scored.length)html='<div class="results-head"><h2>No matches for &ldquo;'+escH(query)+
    '&rdquo;</h2><button onclick="clearResults()">Close</button></div>';
  panel.innerHTML=html;
  panel.style.display='block';
  panel.querySelectorAll('img').forEach(function(img){if(img.loading==='lazy')img.loading='eager';});
  panel.scrollIntoView({behavior:'smooth',block:'start'});
}
// The nav's query box fills itself from the URL (templates/query-box.js). It
// stays on libraries without embeddings, because a query's filters need none;
// a query on the Files page runs through the grid fetch above, which ranks
// its words.
(function(){
  var navInput=document.getElementById('nav-search');
  if(navInput&&typeof HAS_EMBEDDINGS!=='undefined'&&!HAS_EMBEDDINGS&&!navInput.closest('.qbox').querySelector('.qchip')){
    navInput.placeholder='Filter, e.g. tag:deniz…';
  }
})();
if(typeof ALLFILES!=='undefined'){
  ALLFILES.forEach(function(f){
    (HASH_FILES[f.hash]=HASH_FILES[f.hash]||[]).push(f);
  });
  renderGallery();
}else if(document.getElementById('gallery')&&!document.getElementById('map-plot-wrap')){
  // Nothing inlined: a live page. Fetch the first page. The map page is the one
  // exception: it drives the grid through setGalleryLocation once its clusters
  // load (or its plot fails), so gallery.js must not also fire an initial fetch.
  // A search's own page ranks rather than lists.
  if(location.pathname==='/search')runSearchPage(); else renderGallery();
}
document.addEventListener('click',function(e){
  var sb=e.target.closest('[data-similar]');
  if(sb){e.preventDefault();e.stopPropagation();findSimilar(sb.dataset.similar);}
});
// Reflect the stored mode and sort in the controls on load. The initial
// render already honours viewMode() through appendCards / the date path, and
// sortState() through the first fetch (or the sorted inlined array).
document.querySelectorAll('.view-mode-select').forEach(function(s){ s.value=viewMode(); });
reflectSort();
// Row geometry depends on width, so recompute tile layouts on resize (debounced).
var _viewResizeT=null;
window.addEventListener('resize',function(){
  if(viewMode()!=='tile')return;
  clearTimeout(_viewResizeT);
  _viewResizeT=setTimeout(renderCurrentMode,150);
});
// A late width change (fonts, a scrollbar, or an embedding pane sizing itself
// after load) can land the first tile layout at the wrong width, so re-run once
// the page has fully loaded.
window.addEventListener('load',function(){ if(viewMode()==='tile') renderCurrentMode(); });
render(true);
if(typeof GVIEW!=='undefined'&&GVIEW==='events') buildEventsInitialView();
else if(document.getElementById('dateGrid')) buildDateInitialView();

// ---------- select mode ----------
// A Select toggle in the file-grid toolbars (live server only: selections act
// through the server). While on, a click on a card or tile selects it instead
// of opening the lightbox, Shift-click selects the run from the last plain
// click, and the shared selection bar (selection.js) carries the actions.
// Items are keyed by content hash, like marks and tags.
var SELECT_ITEMS='#gallery .card[data-hash], #gallery .tile[data-hash], '+
  '#dateGrid .card[data-hash], #dateGrid .tile[data-hash]';
var selectMode=false, fileSelection=null, selectObserver=null;

function selectionActions(){ return typeof fileSelectionActions==='function'?fileSelectionActions():''; }

function ensureFileSelection(){
  if(fileSelection)return fileSelection;
  fileSelection=createSelection({
    bar:'file-sel-bar',
    items:SELECT_ITEMS,
    keyOf:function(el){ return el.dataset.hash; },
    actions:selectionActions
  });
  return fileSelection;
}

// Handled here, before the lightbox: in select mode an item click selects.
// Buttons and copy-path links inside a card keep their own behaviour.
function selectModeClick(e){
  if(!selectMode)return false;
  var item=e.target.closest(SELECT_ITEMS);
  if(!item||e.target.closest('button, [data-path]'))return false;
  e.preventDefault();e.stopPropagation();
  var sel=ensureFileSelection();
  if(e.shiftKey)sel.extend(item.dataset.hash); else sel.toggle(item.dataset.hash);
  if(typeof loadSelectionTags==='function')loadSelectionTags();
  return true;
}

function setSelectMode(on){
  selectMode=!!on;
  document.body.classList.toggle('select-mode',selectMode);
  document.querySelectorAll('.select-toggle').forEach(function(b){
    b.setAttribute('aria-pressed',selectMode?'true':'false');
  });
  document.querySelectorAll('.select-state').forEach(function(s){ s.hidden=!selectMode; });
  var sel=ensureFileSelection();
  if(!selectMode){ sel.clear(); if(selectObserver){selectObserver.disconnect();selectObserver=null;} return; }
  // Grids re-render wholesale (Show more, List/Tile, sort, a new date); keep
  // the selected look on whatever is drawn now.
  var pending=false;
  selectObserver=new MutationObserver(function(){
    if(pending)return; pending=true;
    requestAnimationFrame(function(){ pending=false; sel.paint(); });
  });
  ['gallery','dateGrid'].forEach(function(id){
    var el=document.getElementById(id);
    if(el)selectObserver.observe(el,{childList:true,subtree:true});
  });
}
function toggleSelectMode(){ setSelectMode(!selectMode); }

(function(){
  if(!LIVE_SERVER)return;
  document.querySelectorAll('.gallery-toolbar[data-files]').forEach(function(bar){
    var b=document.createElement('button');
    b.type='button';
    b.className='select-toggle';
    b.setAttribute('aria-pressed','false');
    b.textContent='Select';
    b.addEventListener('click',toggleSelectMode);
    var state=document.createElement('span');
    state.className='select-state';
    state.hidden=true;
    state.textContent='Select enabled';
    // At the right end of the bar, the state before the button: turning
    // Select on shows "Select enabled" there and moves none of the controls.
    var group=document.createElement('span');
    group.className='select-group';
    group.appendChild(state);
    group.appendChild(b);
    bar.appendChild(group);
  });
})();

// ---------- selection bar actions ----------
// Marks, tags, rotation and copy paths for the selected items. The result of
// the last action stays on the bar until the selection changes.
var SELECTION_LABELS=['red','yellow','green','blue','purple'];

// Whether every selected item is liked, from the rows this page loaded. Decides
// the heart's state: a click then unlikes all rather than likes all.
function selectionAllLiked(){
  var liked={};
  [typeof galleryFiles!=='undefined'?galleryFiles:[],(typeof dateFiles!=='undefined'&&dateFiles)||[]].forEach(function(list){
    (list||[]).forEach(function(f){ if(!(f.hash in liked))liked[f.hash]=!!f.liked; });
  });
  var hs=fileSelection?fileSelection.list():[];
  return hs.length>0&&hs.every(function(h){ return liked[h]; });
}

function fileSelectionActions(){
  var all=selectionAllLiked();
  var heart='<button type="button" class="sel-icon sel-heart" data-sel-act="like" aria-pressed="'+all+'" '+
    'title="'+(all?'Unlike':'Like')+'" aria-label="'+(all?'Unlike':'Like')+'">'+ICON_HEART+'</button>';
  var more=selPopButton('more',ICON_MORE,'More',
    '<button type="button" role="menuitem" data-sel-act="copy" aria-label="Copy paths">Copy paths</button>');
  return heart+selectionMarkControls()+
    '<span class="sel-sep" aria-hidden="true"></span>'+
    '<button type="button" class="sel-icon" data-sel-act="rotate-ccw" title="Rotate left" aria-label="Rotate left">'+ICON_ROT_L+'</button>'+
    '<button type="button" class="sel-icon" data-sel-act="rotate-cw" title="Rotate right" aria-label="Rotate right">'+ICON_ROT_R+'</button>'+
    more+
    '<span class="sel-sep" aria-hidden="true"></span>'+
    '<button type="button" class="sel-icon sel-danger" data-sel-act="delete" title="Delete" aria-label="Delete">'+ICON_TRASH+'</button>';
}
// Rate, label, tag and flag, each a small menu.
var LABEL_COLOURS={red:'#dc2626',yellow:'#eab308',green:'#16a34a',blue:'#2563eb',purple:'#9333ea'};
function selectionMarkControls(){
  var stars=[1,2,3,4,5].map(function(n){
    return '<button type="button" role="menuitem" data-sel-act="rate" data-value="'+n+'" aria-label="'+n+' star'+(n>1?'s':'')+'" title="'+n+'">'+'★'.repeat(n)+'</button>';
  }).join('')+'<button type="button" role="menuitem" data-sel-act="rate" data-value="0" aria-label="Clear rating">Clear</button>';
  var dots=SELECTION_LABELS.map(function(l){
    return '<button type="button" role="menuitem" class="sel-dot" data-sel-act="label" data-value="'+l+'" aria-label="'+l+'" title="'+l+'" style="background:'+LABEL_COLOURS[l]+'"></button>';
  }).join('')+'<button type="button" role="menuitem" data-sel-act="label" data-value="none" aria-label="No label">None</button>';
  var tag='<input type="text" class="sel-tag" list="sel-tag-list" placeholder="Tag" aria-label="Tag">'+
    '<datalist id="sel-tag-list"></datalist>'+
    '<button type="button" data-sel-act="tag" aria-label="Add tag">Add</button>'+
    '<button type="button" data-sel-act="untag" aria-label="Remove tag">Remove</button>';
  var flag='<button type="button" role="menuitem" data-sel-act="keep" aria-label="Keep">Keep</button>'+
    '<button type="button" role="menuitem" data-sel-act="reject" aria-label="Reject">Reject</button>'+
    '<button type="button" role="menuitem" data-sel-act="pick-clear" aria-label="Clear pick">Clear</button>';
  return selPopButton('rate',ICON_STAR,'Rate',stars)+
    selPopButton('label',ICON_LABEL,'Label',dots)+
    selPopButton('tag',ICON_TAG,'Tag',tag)+
    selPopButton('flag',ICON_FLAG,'Keep or reject',flag);
}

// One small menu at a time, opened upward from its button on the bar.
var openPopName=null;
function selPopButton(name,icon,title,body){
  return '<span class="sel-pop-wrap"><button type="button" class="sel-icon" data-sel-pop="'+name+'" title="'+title+'" aria-label="'+title+'" aria-haspopup="true" aria-expanded="false">'+icon+'</button>'+
    '<div class="sel-pop" data-pop="'+name+'" role="menu" hidden>'+body+'</div></span>';
}
function openSelPop(name){
  closeSelPop(false);
  var bar=document.getElementById('file-sel-bar');
  var pop=bar&&bar.querySelector('.sel-pop[data-pop="'+name+'"]');
  if(!pop)return;
  pop.hidden=false;
  bar.querySelector('[data-sel-pop="'+name+'"]').setAttribute('aria-expanded','true');
  openPopName=name;
  if(name==='tag')loadSelectionTags();
  var first=pop.querySelector('input, button');
  if(first)first.focus();
}
function closeSelPop(returnFocus){
  if(!openPopName)return;
  var bar=document.getElementById('file-sel-bar');
  var btn=bar&&bar.querySelector('[data-sel-pop="'+openPopName+'"]');
  var pop=bar&&bar.querySelector('.sel-pop[data-pop="'+openPopName+'"]');
  if(pop)pop.hidden=true;
  if(btn){ btn.setAttribute('aria-expanded','false'); if(returnFocus)btn.focus(); }
  openPopName=null;
}
// Enter in the tag field adds the tag.
document.addEventListener('keydown',function(e){
  if(e.key==='Enter'&&e.target.closest&&e.target.closest('#file-sel-bar .sel-tag')){
    e.preventDefault(); closeSelPop(false); selectionTag(false);
  }
});
// Escape closes an open menu before select mode sees it, so the selection stays.
document.addEventListener('keydown',function(e){
  if(e.key==='Escape'&&openPopName){ e.stopImmediatePropagation(); closeSelPop(true); }
},true);
document.addEventListener('click',function(e){
  var t=e.target.closest('#file-sel-bar [data-sel-pop]');
  if(t){ var n=t.dataset.selPop; if(openPopName===n)closeSelPop(true); else openSelPop(n); return; }
  if(openPopName&&!e.target.closest('#file-sel-bar .sel-pop'))closeSelPop(false);
});

function selectionPost(url,body){
  return fetch(url,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)})
    .then(function(r){
      return r.json().catch(function(){return {};}).then(function(j){
        if(!r.ok)throw new Error(j.error==='library_busy'
          ?'Another videre command is working on this library; try again when it finishes.'
          :(j.field?(j.field+' is invalid'):(j.error||('failed ('+r.status+')'))));
        return j;
      });
    });
}

// The last action's result, shown briefly above the bar. A toast rather than
// bar content, so the bar never changes width or re-renders under an open menu.
function showSelectionResult(text){
  var t=document.getElementById('sel-toast');
  if(!t){
    t=document.createElement('div');
    t.id='sel-toast';
    t.setAttribute('role','status');
    document.body.appendChild(t);
  }
  t.textContent=text;
  t.hidden=false;
  clearTimeout(showSelectionResult.timer);
  showSelectionResult.timer=setTimeout(function(){ t.hidden=true; },4000);
}

function selectionMarks(body,describe){
  var hashes=fileSelection.list();
  body.hashes=hashes;
  selectionPost('/api/files/marks',body).then(function(j){
    if(body.liked!==undefined){
      hashes.forEach(function(h){ syncLikedMeta(h,body.liked); });
      fileSelection.renderBar();
    }
    showSelectionResult(describe(j,hashes.length));
  }).catch(function(e){ showSelectionResult(e.message); });
}
function selectionRate(n){
  selectionMarks({rating:n},function(_,c){ return n?('Rated '+c+' item(s) '+'★'.repeat(n)):('Cleared the rating of '+c+' item(s)'); });
}
function selectionLabel(l){
  selectionMarks({label:l},function(_,c){ return l==='none'?('Cleared the label of '+c+' item(s)'):('Labeled '+c+' item(s) '+l); });
}

function selectionTag(remove){
  var input=document.querySelector('#file-sel-bar .sel-tag');
  var tag=input?input.value.trim():'';
  if(!tag){ showSelectionResult('Type a tag first.'); return; }
  var hashes=fileSelection.list();
  var body={hashes:hashes};
  body[remove?'remove':'add']=[tag];
  selectionPost('/api/files/tags',body).then(function(){
    showSelectionResult((remove?'Untagged ':'Tagged ')+hashes.length+' item(s) '+(remove?'from ':'with ')+tag);
  }).catch(function(e){ showSelectionResult(e.message); });
}

function selectionRotate(direction){
  var hashes=fileSelection.list();
  selectionPost('/api/files/rotate',{hashes:hashes,direction:direction}).then(function(j){
    // Reload the turned thumbnails and the URLs their lightbox opens; the
    // server has already dropped their cache.
    var token='b='+Date.now();
    hashes.forEach(function(h){ refreshTilesFor(h,token); });
    var text='Rotated '+j.rotated+' item(s)';
    if(j.skipped)text+=', skipped '+j.skipped+' that cannot be rotated (videos, RAW)';
    if(j.failed)text+=', '+j.failed+' failed';
    showSelectionResult(text);
  }).catch(function(e){ showSelectionResult(e.message); });
}

function selectionCopyPaths(){
  var rows={};
  [galleryFiles,(typeof dateFiles!=='undefined'&&dateFiles)||[]].forEach(function(list){
    (list||[]).forEach(function(f){ if(!rows[f.hash])rows[f.hash]=f.path; });
  });
  var paths=fileSelection.list().map(function(h){return rows[h];}).filter(Boolean);
  copyPath(paths.join('\n'));
  showSelectionResult('Copied '+paths.length+' path(s)');
}

function loadSelectionTags(){
  var list=document.getElementById('sel-tag-list');
  if(!list)return;
  fetch('/api/tags').then(function(r){return r.ok?r.json():[];}).then(function(tags){
    list.innerHTML=tags.map(function(t){return '<option value="'+escA(t.tag)+'">';}).join('');
  }).catch(function(){});
}

document.addEventListener('click',function(e){
  var btn=e.target.closest('#file-sel-bar [data-sel-act]');
  if(!btn||!fileSelection)return;
  closeSelPop(false);
  switch(btn.dataset.selAct){
    case 'like':
      var unlike=selectionAllLiked();
      selectionMarks({liked:!unlike},function(_,c){return (unlike?'Unliked ':'Liked ')+c+' item(s)';});
      break;
    case 'keep': selectionMarks({pick:'keep'},function(j,c){return j.pick==='none'?('Cleared Keep on '+c+' item(s)'):('Marked '+c+' item(s) Keep');}); break;
    case 'reject': selectionMarks({pick:'reject'},function(j,c){return j.pick==='none'?('Cleared Reject on '+c+' item(s)'):('Marked '+c+' item(s) Reject');}); break;
    case 'rate': selectionRate(Number(btn.dataset.value)); break;
    case 'label': selectionLabel(btn.dataset.value); break;
    case 'pick-clear': selectionMarks({pick:'none'},function(_,c){return 'Cleared the pick of '+c+' item(s)';}); break;
    case 'tag': selectionTag(false); break;
    case 'untag': selectionTag(true); break;
    case 'rotate-ccw': selectionRotate('ccw'); break;
    case 'rotate-cw': selectionRotate('cw'); break;
    case 'copy': selectionCopyPaths(); break;
    case 'delete': selectionDelete(); break;
  }
});

// Delete: count first (a dry run of the same request), say exactly what will
// move, and only then move it. Files go to the system Trash, every copy of
// each item.
function selectionDelete(){
  var hashes=fileSelection.list();
  selectionPost('/api/files/delete',{hashes:hashes,dry_run:true}).then(function(c){
    confirmDelete(c).then(function(ok){
      if(!ok)return;
      selectionPost('/api/files/delete',{hashes:hashes,dry_run:false}).then(function(r){
        var gone={};
        (r.trashed||[]).forEach(function(h){ gone[h]=true; });
        var before=galleryFiles.length;
        galleryFiles=galleryFiles.filter(function(f){ return !gone[f.hash]; });
        // The next Show more pages from the server's shorter list.
        gShown=Math.max(0,gShown-(before-galleryFiles.length));
        if(typeof dateFiles!=='undefined'&&dateFiles){
          var dateBefore=dateFiles.length;
          dateFiles=dateFiles.filter(function(f){ return !gone[f.hash]; });
          // Likewise the date's next page comes from its shorter list.
          dateTotal-=dateBefore-dateFiles.length;
          updateDateMore();
        }
        renderCurrentMode();
        fileSelection.remove(r.trashed||[]);
        var text='Moved '+(r.trashed||[]).length+' item(s) to the Trash';
        if(r.failed&&r.failed.length)text+='; '+r.failed.length+' file(s) could not be moved ('+r.failed[0].error+')';
        showSelectionResult(text);
      }).catch(function(e){ showSelectionResult(e.message); });
    });
  }).catch(function(e){ showSelectionResult(e.message); });
}

function confirmDelete(c){
  return new Promise(function(resolve){
    var parts=[];
    if(c.photos)parts.push(c.photos+' photo'+(c.photos===1?'':'s'));
    if(c.videos)parts.push(c.videos+' video'+(c.videos===1?'':'s'));
    var copies=c.extra_copies?' and '+c.extra_copies+' extra cop'+(c.extra_copies===1?'y':'ies')+' of items with duplicates':'';
    var d=document.createElement('dialog');
    d.className='sel-confirm';
    d.innerHTML='<h2>Move '+c.items+' item'+(c.items===1?'':'s')+' to the Trash?</h2>'+
      '<p>'+c.files+' file'+(c.files===1?'':'s')+': '+escH(parts.join(', '))+copies+'.</p>'+
      '<p>They go to the system Trash and can be restored from there. Their marks, tags and faces stay until <code>videre prune</code> clears data for missing files.</p>'+
      '<div class="sel-confirm-actions"><button type="button" data-no autofocus>Cancel</button>'+
      '<button type="button" data-yes class="primary">Move to Trash</button></div>';
    document.body.appendChild(d);
    function done(ok){ d.close(); d.remove(); resolve(ok); }
    d.querySelector('[data-no]').addEventListener('click',function(){ done(false); });
    d.querySelector('[data-yes]').addEventListener('click',function(){ done(true); });
    d.addEventListener('cancel',function(e){ e.preventDefault(); done(false); });
    d.showModal();
  });
}

// Back to top: a small button at the bottom right of every grid page, shown
// once the page has scrolled past one screen, for long tile and list views.
(function(){
  var b=document.createElement('button');
  b.type='button';
  b.className='to-top';
  b.title='Back to top';
  b.setAttribute('aria-label','Back to top');
  b.innerHTML=ICON_UP;
  b.addEventListener('click',function(){ window.scrollTo({top:0,behavior:'smooth'}); });
  function sync(){ b.hidden=window.scrollY<window.innerHeight; }
  window.addEventListener('scroll',sync,{passive:true});
  document.body.appendChild(b);
  sync();
})();
