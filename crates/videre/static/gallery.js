
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
// Small leading icons for the info rows (stroke uses currentColor, sized in CSS).
const ICON_FILE='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6"/></svg>';
const ICON_DATE='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="18" rx="2"/><path d="M8 2v4M16 2v4M3 10h18"/></svg>';
const ICON_SIZE='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6"/><path d="M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3"/></svg>';
const ICON_PIN='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 10c0 6-8 12-8 12s-8-6-8-12a8 8 0 0 1 16 0z"/><circle cx="12" cy="10" r="3"/></svg>';
// Heart for the like toggle. One path; the CSS decides outline vs filled red by
// the button's `liked` class (fill:none + stroke, or fill:red).
const ICON_HEART='<svg viewBox="0 0 24 24" stroke-linejoin="round" stroke-linecap="round"><path d="M12 21C12 21 3 14.5 3 8.5 3 5.42 5.42 3 8.5 3c1.74 0 3.41 0.81 4.5 2.09C14.09 3.81 15.76 3 17.5 3 20.58 3 23 5.42 23 8.5 23 14.5 12 21 12 21Z" transform="translate(-1 0)"/></svg>';
function renderMetaPanel(meta){
  const el = document.getElementById('lbMeta');
  // Shown under the media. The left column carries the file facts; a right
  // column of people is added only when the file has labeled faces, so a file
  // with none stays a single full-width column.
  el.classList.add('on');
  if(!meta){ el.innerHTML=''; return; }
  const rows = [];
  if(meta.name) rows.push('<div class="lb-row lb-fname">'+ICON_FILE+'<span>'+escH(meta.name)+'</span></div>');
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
  document.querySelectorAll('[data-lb-meta]').forEach(function(el){
    try{
      var m=JSON.parse(el.dataset.lbMeta);
      if(m && m.hash===hash){ m.liked=liked; el.dataset.lbMeta=JSON.stringify(m); }
    }catch(e){}
  });
}
var lbIndex=-1;      // index of the open item among the currently visible tiles
var lbLoading=false; // guards the auto-paginate load so it fires once
// The formats whose EXIF orientation the rotate button can edit; must match
// videre gallery's rotate endpoint (supports_exif_orientation).
var ROTATABLE_EXTS=['jpg','jpeg','png','tif','tiff','webp'];
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
  // Rotate is offered only for EXIF-bearing images: the endpoint refuses the
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
// Refresh every on-page thumbnail for one hash after its orientation changed,
// so the grid tile turns with the lightbox rather than lagging until reload.
function refreshTilesFor(hash,token){
  var needle='/api/files/'+encodeURIComponent(hash)+'/raw';
  var imgs=document.querySelectorAll('img');
  for(var i=0;i<imgs.length;i++){
    if(imgs[i].id==='lb-img')continue;
    if(imgs[i].src&&imgs[i].src.indexOf(needle)>=0)imgs[i].src=bustUrl(imgs[i].src,token);
  }
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
      document.getElementById('lb-img').src=bustUrl(lbCurrent.url,token);
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
  lbImg().src='/api/files/'+encodeURIComponent(lbCurrent.hash)+'/raw';
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
// grid/duplicates views, #gallery-more for the paged gallery.
function lbMoreButton(){
  var ids=['more-btn','gallery-more'];
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
// The files last rendered into #dateGrid, so a mode switch re-renders without a
// refetch. Date galleries are one-shot (no paging).
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
  if(LIVE_SERVER) return '<a class="date-card-link" href="/date/'+escA(String(prefix).replace(/-/g,'/'))+'" aria-label="Open '+escA(prefix)+'"></a>';
  return '<a class="date-card-link" href="#" onclick="'+action+';return false" aria-label="Open '+escA(prefix)+'"></a>';
}
function dateCrumb(label,prefix,action){
  if(LIVE_SERVER) return '<a href="'+escA(dateHref(prefix))+'">'+escH(label)+'</a>';
  return '<a onclick="'+action+'">'+escH(label)+'</a>';
}
// The topmost breadcrumb, back to the full year overview: the /date route on a
// live server, or buildYearView() in a static export.
function dateRootCrumb(){
  return LIVE_SERVER ? '<a href="/date">All Dates</a>' : '<a onclick="buildYearView()">All Dates</a>';
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
  fetch(q).then(function(r){return r.json();})
    .then(function(d){ then(d.buckets||[]); })
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
  dateState={level:'year',year:null,month:null};
  document.getElementById('dateBreadcrumb').innerHTML='All Dates';
  var narrowing=document.getElementById('dateNarrowing');
  if(narrowing)narrowing.innerHTML='';
  var draw=function(b){
    document.getElementById('dateGrid').innerHTML=
      dateCards(b,function(k){return "buildMonthView('"+k+"')";});
  };
  if(dateInlined())draw(groupInlined(4,null)); else fetchBuckets('year',null,draw);
}
function buildMonthView(year){
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
    renderDateFiles(files,'No files for '+day+'.');
    return;
  }
  grid.innerHTML='<p class="muted">Loading\u2026</p>';
  fetch('/api/files?view=date&date='+encodeURIComponent(day)+'&limit=500')
    .then(function(r){return r.json();})
    .then(function(d){
      showPeriodCount(d.total!=null?d.total:(d.files||[]).length);
      renderDateFiles(d.files||[],'No files for '+day+'.');
    })
    .catch(function(){ grid.innerHTML='<p class="muted">Could not load that day.</p>'; });
}
function fetchDateFiles(params,emptyText){
  var grid=document.getElementById('dateGrid');
  grid.innerHTML='<p class="muted">Loading...</p>';
  fetch('/api/files?view=date&'+params+'&limit=500')
    .then(function(r){return r.json();})
    .then(function(d){
      showPeriodCount(d.total!=null?d.total:(d.files||[]).length);
      renderDateFiles(d.files||[],emptyText);
    })
    .catch(function(){ grid.innerHTML='<p class="muted">Could not load that date.</p>'; });
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
  var parts=prefix.split('-');
  dateState.year=parts[0]||null;
  dateState.month=parts.length>=2 ? parts[0]+'-'+parts[1] : null;
  renderDateBreadcrumb(prefix);
  renderDateNarrowing(prefix);
  fetchDateFiles('date='+encodeURIComponent(prefix),'No files for '+prefix+'.');
}
function buildRangeGallery(range){
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
      '<a class="date-card-link" href="/events/'+escA(e.key)+'" aria-label="Open '+escA(label)+'"></a>'+
      buildPreview(e.sample)+
      '<div class="date-card-label">'+escH(label)+'</div>'+
      '<div class="date-card-count">'+sub+'</div></div>';
  }).join('');
}
function buildEventsOverview(){
  document.getElementById('dateBreadcrumb').innerHTML='Events';
  var narrowing=document.getElementById('dateNarrowing');
  if(narrowing)narrowing.innerHTML='';
  var grid=document.getElementById('dateGrid');
  grid.innerHTML='<p class="muted">Loading…</p>';
  fetch('/api/events').then(function(r){return r.json();})
    .then(function(d){
      var evs=d.events||[];
      var reasons={
        no_media:'No media has been scanned yet.',
        no_capture_dates:'Events needs photo capture dates or video creation dates.',
        insufficient_location_evidence:'Events needs dated, location-supported photos to recognize travel.',
        no_qualifying_trips:'No substantial travel trips were found yet.'
      };
      grid.innerHTML=evs.length?eventCards(evs):
        '<div class="empty-state"><h2>No travel trips yet</h2><p>'+escH(reasons[d.empty_reason]||reasons.no_qualifying_trips)+
        '</p><p class="hint">Events finds travel from dated, location-supported photos.</p></div>';
    })
    .catch(function(){ grid.innerHTML='<p class="muted">Could not load events.</p>'; });
}
function buildEventLeaf(ev){
  var range=eventDateRange(ev.from,ev.to);
  document.getElementById('dateBreadcrumb').innerHTML=
    '<a href="/events">All Events</a> &gt; '+escH(ev.title||range);
  var narrowing=document.getElementById('dateNarrowing');
  if(narrowing)narrowing.innerHTML='';
  var grid=document.getElementById('dateGrid');
  grid.innerHTML='<p class="muted">Loading…</p>';
  fetch('/api/events/'+encodeURIComponent(ev.key)+'/files')
    .then(function(r){return r.json();})
    .then(function(d){
      showPeriodCount(d.total!=null?d.total:(d.files||[]).length);
      renderDateFiles(d.files||[],'No files in this event.');
    })
    .catch(function(){ grid.innerHTML='<p class="muted">Could not load this event.</p>'; });
}
function buildEventsInitialView(){
  if(typeof GEVENT==='object'&&GEVENT) buildEventLeaf(GEVENT);
  else buildEventsOverview();
}
// Event delegation: toggle, lightbox, copy. One listener for all dynamic content
document.addEventListener('click',function(e){
  var lb=e.target.closest('[data-lb-url]');
  if(lb){e.preventDefault();e.stopPropagation();openTile(lb);return;}
  var cp=e.target.closest('[data-path]');
  if(cp){copyPath(cp.dataset.path);return;}
  var hdr=e.target.closest('.group-header');
  if(hdr){toggle(hdr.closest('.group').id);return;}
});
document.addEventListener('keydown',function(e){
  if(e.key==='Escape'){closeLb();return;}
  if(!document.getElementById('lb').classList.contains('on'))return;
  if(e.key==='ArrowLeft'){e.preventDefault();lbStep(-1);}
  else if(e.key==='ArrowRight'){e.preventDefault();lbStep(1);}
});
document.getElementById('lb').addEventListener('click',function(e){
  if(e.target===this)closeLb();
});

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
  return '<div class="tile" style="left:'+box.left+'px;top:'+box.top+'px;'+
    'width:'+box.width+'px;height:'+box.height+'px">'+buildPreview(f)+'</div>';
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
// and a later mode switch share a path. Date galleries are one-shot, so this
// replaces the grid contents wholesale.
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
    appendCards(ALLFILES.slice(gShown,gShown+GPAGE),ALLFILES.length);
    return;
  }
  if(gLoading)return;
  gLoading=true;
  var request=++gRequest;
  var btn=document.getElementById('gallery-more');
  if(btn)btn.textContent='Loading\u2026';
  fetch('/api/files?view='+encodeURIComponent(GVIEW)+'&offset='+gShown+'&limit='+GPAGE+
        galleryLocationQuery())
    .then(function(r){ return r.json(); })
    .then(function(d){
      if(request!==gRequest)return;
      gLoading=false;
      appendCards(d.files||[],d.total||0);
    })
    .catch(function(){
      if(request!==gRequest)return;
      gLoading=false;
      // Say so, rather than leaving a button that silently does nothing.
      if(btn)btn.textContent='Could not load more. Click to retry.';
    });
}
function showMoreGallery(){renderGallery();}
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
        '<button onclick="clearResults()">Clear</button></div>';
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
    '<button onclick="clearResults()">Clear</button></div>'+
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
        '<button onclick="clearResults()">Clear</button></div>';
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
    '<button onclick="clearResults()">Clear</button></div><div class="results-strip">';
  for(var i=0;i<scored.length;i++)html+=resultCard(scored[i].hash,scored[i].score,false);
  html+='</div>';
  if(!scored.length)html='<div class="results-head"><h2>No matches for &ldquo;'+escH(query)+
    '&rdquo;</h2><button onclick="clearResults()">Clear</button></div>';
  panel.innerHTML=html;
  panel.style.display='block';
  panel.querySelectorAll('img').forEach(function(img){if(img.loading==='lazy')img.loading='eager';});
  panel.scrollIntoView({behavior:'smooth',block:'start'});
}
// Wire the shared nav search box: prefill it from the URL, hide it where the
// library has no embeddings to rank against, and run any `?q=` on the Files page.
(function(){
  if(typeof HAS_EMBEDDINGS!=='undefined'&&!HAS_EMBEDDINGS){
    var form=document.querySelector('.secnav-search');
    if(form)form.style.display='none';
    return;
  }
  var q=new URLSearchParams(window.location.search).get('q');
  if(!q)return;
  var navInput=document.getElementById('nav-search');
  if(navInput)navInput.value=q;
  if(typeof LIVE_SERVER!=='undefined'&&LIVE_SERVER&&document.getElementById('results'))runTextSearch(q);
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
  renderGallery();
}
document.addEventListener('click',function(e){
  var sb=e.target.closest('[data-similar]');
  if(sb){e.preventDefault();e.stopPropagation();findSimilar(sb.dataset.similar);}
});
// Reflect the stored mode in the toggle(s) on load. The initial render already
// honours viewMode() through appendCards / the date path.
document.querySelectorAll('.view-mode-select').forEach(function(s){ s.value=viewMode(); });
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
