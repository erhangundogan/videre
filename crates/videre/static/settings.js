// Gallery settings runtime, inlined into every page by templates/nav.html
// after the globals it reads: VIDERE_SETTINGS (the effective document),
// VIDERE_SETTINGS_DEFAULTS, VIDERE_SETTINGS_LIVE and VIDERE_SETTINGS_ERROR.
// See commands/gallery/settings.rs. Readers validate where they read and fall
// back to the default, because the server does not know what any setting
// means.
function settingAt(doc,path){
  var parts=path.split('.');
  for(var i=0;i<parts.length;i++){
    if(doc===null||typeof doc!=='object')return undefined;
    doc=doc[parts[i]];
  }
  return doc;
}
function settingDefault(path){ return settingAt(VIDERE_SETTINGS_DEFAULTS,path); }
function setting(path){
  var v=settingAt(VIDERE_SETTINGS,path);
  return v===undefined?settingDefault(path):v;
}
function settingOneOf(path,values){
  var v=setting(path);
  return values.indexOf(v)>=0?v:settingDefault(path);
}
function settingInRange(path,min,max){
  var v=setting(path);
  return (typeof v==='number'&&isFinite(v)&&v>=min&&v<=max)?v:settingDefault(path);
}
var settingsPatch=null,settingsTimer=null;
// Applies to this page at once and saves after a quiet 300 ms, so a burst of
// changes writes once. A static export has no server: the change lasts the
// session only.
function saveSetting(path,value){
  var parts=path.split('.'),doc=VIDERE_SETTINGS,patch;
  settingsPatch=settingsPatch||{};
  patch=settingsPatch;
  for(var i=0;i<parts.length-1;i++){
    if(doc[parts[i]]===null||typeof doc[parts[i]]!=='object')doc[parts[i]]={};
    doc=doc[parts[i]];
    if(patch[parts[i]]===null||typeof patch[parts[i]]!=='object')patch[parts[i]]={};
    patch=patch[parts[i]];
  }
  doc[parts[parts.length-1]]=value;
  patch[parts[parts.length-1]]=value;
  if(!VIDERE_SETTINGS_LIVE){ settingsPatch=null; return; }
  clearTimeout(settingsTimer);
  settingsTimer=setTimeout(flushSettings,300);
}
function flushSettings(){
  clearTimeout(settingsTimer);
  if(!settingsPatch)return;
  var body=JSON.stringify(settingsPatch);
  settingsPatch=null;
  // keepalive lets a save started just before navigating away still land.
  fetch('/api/settings',{method:'PATCH',keepalive:true,
    headers:{'Content-Type':'application/merge-patch+json'},body:body}).catch(function(){});
}
window.addEventListener('pagehide',flushSettings);
// A settings file that exists but cannot be read is never overwritten, so
// say why choices are not being kept rather than dropping them silently.
if(VIDERE_SETTINGS_ERROR){
  document.addEventListener('DOMContentLoaded',function(){
    var b=document.createElement('div');
    b.className='settings-banner';
    b.setAttribute('role','alert');
    b.textContent='Gallery settings not loaded, using defaults: '+VIDERE_SETTINGS_ERROR+
      '. Fix or delete the file to save settings again.';
    document.body.insertBefore(b,document.body.firstChild);
  });
}
