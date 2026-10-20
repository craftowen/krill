// Add to Home Screen handoff in headless Chrome against a real krilld (called from mobile_pwa.cjs).
// Safari pairs, hands a one-time pairing fragment to a fresh "Home Screen" storage, and nothing secret reaches a URL or manifest.
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),os=require('node:os'),path=require('node:path'),{spawn}=require('node:child_process');
const CHROME=process.env.CHROME_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const IPHONE='Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1';
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function until(fn,what){const end=Date.now()+15000;let last;while(Date.now()<end){try{const value=await fn();if(value)return value;}catch(e){last=e;}await sleep(50);}throw Error(`timed out: ${what}${last?' ('+last.message+')':''}`);}

async function browser(){
 const profile=fs.mkdtempSync(path.join(os.tmpdir(),'krill-chrome-')),chrome=spawn(CHROME,['--headless=new','--remote-debugging-port=0',`--user-data-dir=${profile}`,'--no-first-run','--no-default-browser-check','about:blank'],{stdio:['ignore','ignore','pipe']});
 const endpoint=await new Promise((resolve,reject)=>{let text='';chrome.stderr.on('data',b=>{text+=b;const m=/DevTools listening on (ws:\S+)/.exec(text);if(m)resolve(m[1]);});chrome.once('exit',()=>reject(Error('Chrome exited: '+text.slice(-500))));});
 const socket=new WebSocket(endpoint);await new Promise((resolve,reject)=>{socket.onopen=resolve;socket.onerror=reject;});
 let id=0;const pending=new Map(),handlers=[];
 socket.onmessage=e=>{const m=JSON.parse(e.data);if(m.id&&pending.has(m.id)){const p=pending.get(m.id);pending.delete(m.id);m.error?p.reject(Error(m.error.message)):p.resolve(m.result);}else for(const h of handlers)h(m);};
 const send=(method,params={},sessionId)=>new Promise((resolve,reject)=>{const n=++id;pending.set(n,{resolve,reject});socket.send(JSON.stringify({id:n,method,params,sessionId}));});
 const traffic=[];handlers.push(m=>{const p=m.params||{};
  if(m.method==='Network.requestWillBeSent')traffic.push(p.request.url,JSON.stringify(p.request.headers),p.request.postData||'');
  if(m.method==='Network.webSocketCreated')traffic.push(p.url);
  if(m.method==='Network.webSocketWillSendHandshakeRequest')traffic.push(JSON.stringify(p.request.headers));
  if(m.method==='Network.webSocketFrameSent')traffic.push('frame:'+p.response.payloadData);});
 async function page({iphone=false,homeScreen=false,isolated=true}={}){
  // Each isolated context has its own storage, like Safari vs an iOS Home Screen app. Installability needs the default (non-incognito) one.
  const {browserContextId}=isolated?await send('Target.createBrowserContext'):{},{targetId}=await send('Target.createTarget',{url:'about:blank',browserContextId}),{sessionId}=await send('Target.attachToTarget',{targetId,flatten:true});
  const call=(method,params)=>send(method,params,sessionId);await call('Network.enable');await call('Page.enable');await call('Runtime.enable');
  if(iphone)await call('Emulation.setUserAgentOverride',{userAgent:IPHONE,platform:'iPhone'});
  // A Home Screen launch: standalone display and its own (fresh) storage.
  if(homeScreen)await call('Page.addScriptToEvaluateOnNewDocument',{source:"Object.defineProperty(Navigator.prototype,'standalone',{get:()=>true})"});
  const evaluate=async expression=>{const r=await call('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});if(r.exceptionDetails)throw Error(r.exceptionDetails.exception?.description||r.exceptionDetails.text);return r.result.value;};
  const open=async url=>{await call('Page.navigate',{url:'about:blank'});await call('Page.navigate',{url});await until(()=>evaluate("document.readyState==='complete'&&!!globalThis.KrillMobile?.app"),'app loaded '+url.split('#')[0]);};
  const click=label=>evaluate(`(()=>{const b=[...document.querySelectorAll('button')].find(b=>b.getAttribute('aria-label')===${JSON.stringify(label)}&&b.offsetParent!==null);if(!b)return false;b.click();return true;})()`);
  const stored=()=>evaluate("JSON.parse(localStorage.getItem('minikrill-mobile-v2')||'null')");
  const ready=()=>evaluate("KrillMobile.store.connection.phase==='ready'");
  return {call,evaluate,open,click,stored,ready};
 }
 return {send,page,traffic,async close(){
  const exited=chrome.exitCode!==null||chrome.signalCode!==null?Promise.resolve():new Promise(r=>chrome.once('exit',r));
  // Browser.close는 렌더러·프로필 쓰기까지 정리한다. 부모만 SIGKILL하면 남은 자식과 rmdir가 경쟁한다.
  const forced=setTimeout(()=>{chrome.kill('SIGKILL');socket.close();},5000);
  try{if(socket.readyState===WebSocket.OPEN)await Promise.race([send('Browser.close'),exited]);else chrome.kill('SIGTERM');await exited;}
  finally{clearTimeout(forced);socket.close();fs.rmSync(profile,{recursive:true,force:true,maxRetries:5,retryDelay:100});}
 }};
}

exports.main=async function main({base,fragment}){
 if(!fs.existsSync(CHROME)){console.log('SKIP Home Screen handoff: Chrome not found (set CHROME_BIN)');return;}
 const chrome=await browser();
 try{
  // Manifest stays valid and installable for Chrome/Android (no handoff needed there: the installed app shares Chrome's storage).
  const android=await chrome.page({isolated:false});await android.open(base+'/');
  const manifest=await until(async()=>{const m=await android.call('Page.getAppManifest');return m.url&&m;},'manifest');
  assert.deepEqual(manifest.errors,[]);const parsed=JSON.parse(manifest.data);assert.equal(parsed.display,'standalone');assert.equal(parsed.start_url,'/');
  const {installabilityErrors}=await android.call('Page.getInstallabilityErrors');assert.deepEqual(installabilityErrors,[],JSON.stringify(installabilityErrors));
  assert.equal(await android.evaluate("document.querySelector('link[rel=apple-touch-icon]').href.endsWith('/apple-touch-icon.png')"),true);

  // Safari: pair, then Settings → Add to Home Screen puts a one-time pairing link into the URL fragment only.
  const safari=await chrome.page({iphone:true});await safari.open(`${base}/#${fragment}`);
  assert.equal(await safari.evaluate('location.hash'),'#/pair-confirm');
  assert.equal(await safari.evaluate("document.querySelector('link[rel=manifest]')"),null,'iOS must keep the current URL, not a manifest start_url');
  assert(await safari.click('Pair'));const browserDevice=await until(safari.stored,'Safari pairing');await until(safari.ready,'Safari ready');
  assert.equal(await until(()=>safari.evaluate("document.getElementById('action-sheet').open&&document.querySelector('#sheet-content h2').textContent"),'post-pairing offer'),'Add Krill to your Home Screen?');
  assert(await safari.click('Cancel'));await safari.evaluate("location.hash='#/settings'");
  assert(await until(()=>safari.click('Add to Home Screen'),'settings row'));
  const handoff=await until(()=>safari.evaluate("location.hash.includes('&install=1')&&location.href"),'handoff fragment');
  assert.match(new URL(handoff).hash,/^#pair=[0-9a-f]{64}&key=[\w-]{43}&install=1$/);assert.equal(new URL(handoff).search,'');
  assert.match(await safari.evaluate("document.querySelector('#sheet-content').textContent"),/Share.*Add to Home Screen.*Within 5 minutes/);

  // Home Screen first launch: own storage, fragment wiped, one tap pairs a new device.
  const home=await chrome.page({iphone:true,homeScreen:true});await home.open(handoff);
  assert.equal(await home.evaluate('location.hash'),'#/pair-confirm');assert.equal(await home.stored(),null);
  assert(await home.click('Pair'));const homeDevice=await until(home.stored,'Home Screen pairing');await until(home.ready,'Home Screen ready');
  assert.notEqual(homeDevice.device_id,browserDevice.device_id);assert.notEqual(homeDevice.credential,browserDevice.credential);
  assert.doesNotMatch(await home.evaluate('location.href'),/pair=|key=|install/);assert.equal(await home.evaluate("document.getElementById('action-sheet').open"),false);
  // Later launches reopen the same start URL: the spent link is ignored and the saved device reconnects.
  await home.open(handoff);await until(home.ready,'Home Screen relaunch');
  assert.deepEqual(await home.stored(),homeDevice);assert.doesNotMatch(await home.evaluate('location.href'),/pair=|key=|install/);
  // The spent ticket cannot pair anyone else.
  const thief=await chrome.page({iphone:true});await thief.open(handoff);assert(await thief.click('Pair'));
  await until(()=>thief.evaluate("/rejected/.test(KrillMobile.store.connection.message)"),'spent ticket rejected');assert.equal(await thief.stored(),null);

  // No credential, pairing key or ticket ever left the page in a URL, header, body or the manifest; WS frames never carry a credential or key.
  const fields=new URLSearchParams(new URL(handoff).hash.slice(1)),start=new URLSearchParams(fragment);
  const secrets=[browserDevice.credential,homeDevice.credential,fields.get('key'),start.get('key')],ids=[fields.get('pair'),start.get('pair')];
  assert(chrome.traffic.length>10);
  for(const item of chrome.traffic)for(const secret of [...secrets,...(item.startsWith('frame:')?[]:ids)])assert(!item.includes(secret),'secret leaked: '+item.slice(0,120));
  for(const secret of [...secrets,...ids])assert(!manifest.data.includes(secret));
  console.log(`PASS Home Screen handoff (headless Chrome): installable manifest, iOS fragment-only one-time pairing → separate device, fragment wiped, relaunch reuses device, spent ticket rejected, no secret in ${chrome.traffic.length} requests/frames`);
 }finally{await chrome.close();}
};
