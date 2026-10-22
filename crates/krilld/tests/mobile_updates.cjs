// Version transition contracts with real bundle hashes, storage and endpoint code; no user devices.
'use strict';
const test=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm'),crypto=require('node:crypto');
const dir=path.join(__dirname,'../src/mobile/assets'),read=name=>fs.readFileSync(path.join(dir,name),'utf8');
const client=read('client.js'),updates=read('updates.js'),oldHash='0123456789abcdef',rid='a'.repeat(32);
const inventory=/<meta name="krill-update" content="1 ([^"]+)">/.exec(read('index.html'))[1].split(' ').sort();
function bundle(change={}){
 const files=new Map(inventory.map(name=>[name,fs.readFileSync(path.join(dir,name))]));
 files.set('app.js',Buffer.concat([files.get('app.js'),Buffer.from('\n// next version\n')]));
 for(const [name,value] of Object.entries(change))files.set(name,Buffer.from(value));
 const sha=crypto.createHash('sha256');for(const [name,bytes] of [...files].sort(([a],[b])=>a.localeCompare(b)))sha.update(`${name}\n${bytes.length}\n`).update(bytes);
 return {files,hash:sha.digest('hex').slice(0,16)};
}
const flush=async()=>{for(let i=0;i<40;i++)await Promise.resolve();await new Promise(setImmediate);};
function fixture(options={}){
 const optionsFixture=options;
 let now=100000,id=0,busy=false,blocked=false,next=bundle(options.change),found={hash:next.hash,relay:true,url:''},failFile,failCache=false,failState=false,failDestination=false;
 const timers=new Map(),requests=[],redirects=[],cache=new Map(),session=new Map(),store=new Map([['krill-mobile-v2',JSON.stringify({device_id:'device-A',credential:'c'.repeat(64),rid,hash:oldHash})]]),doc=Object.assign(new EventTarget(),{hidden:false}),events=new EventTarget();
 const location=new URL(`https://phone.test/m/${oldHash}/?session=7#/host/local/group/project/s/7`);location.replace=url=>redirects.push(url);
 const C={enc:new TextEncoder(),concat:(...parts)=>Uint8Array.from(Buffer.concat(parts)),sha256:bytes=>Uint8Array.from(crypto.createHash('sha256').update(bytes).digest()),hex:bytes=>Buffer.from(bytes).toString('hex')};
 const context=vm.createContext({URL,URLSearchParams,AbortController,TextEncoder,TextDecoder,Uint8Array,Response,Event,Map,Set,MkCrypto:C,document:doc,navigator:{serviceWorker:Object.assign(new EventTarget(),{controller:{},register:async()=>{}})},location,history:{replaceState(_a,_b,url){location.href=new URL(url,location).href;}},Date:class extends Date{static now(){return now;}},setTimeout:(fn,ms)=>{const key=++id;timers.set(key,{fn,at:now+ms});return key;},clearTimeout:key=>timers.delete(key),addEventListener:events.addEventListener.bind(events),removeEventListener:events.removeEventListener.bind(events),
  localStorage:{getItem:key=>store.get(key)||null,setItem:(key,value)=>{if(failDestination&&key==='krill-mobile-v2')throw Error('quota');store.set(key,value);}},
  sessionStorage:{getItem:key=>session.get(key)||null,setItem:(key,value)=>{if(failState)throw Error('quota');session.set(key,value);},removeItem:key=>session.delete(key)},
  caches:{open:async()=>({put:async(key,value)=>{if(failCache)throw Error('quota');cache.set(key,value);},keys:async()=>[...cache.keys()].map(url=>({url})),delete:async request=>cache.delete(request.url)})},
  fetch:async(url,options)=>{requests.push({url,options});if(url.startsWith('/r/'))return new Response(JSON.stringify(found));const cacheKey=new URL(url,location.origin).href;if(optionsFixture.swCache&&cache.has(cacheKey))return cache.get(cacheKey).clone();const name=url.endsWith('/')?'index.html':url.split(`/m/${next.hash}/`)[1];if(name===failFile)return new Response('Missing',{status:404});return new Response(next.files.get(name)||'wrong bundle');}
 });
 vm.runInContext(client,context);vm.runInContext(updates,context);const endpoint=context.KrillMobile.transport.bootstrap();
 const state={drafts:[[7,'작성 중 메시지']],terminalDrafts:[[7,'printf test']],ui:{search:'project',filter:'attention'}};let restored;
 const api=context.KrillMobile.updates.create({endpoint,blocked:()=>blocked,busy:()=>busy,capture:()=>state,restore:saved=>restored=saved});
 async function advance(ms){now+=ms;for(const [key,timer] of [...timers])if(timer.at<=now){timers.delete(key);timer.fn();}await flush();}
 return {context,api,endpoint,doc,events,requests,redirects,cache,session,store,state,advance,get restored(){return restored;},get hash(){return next.hash;},set busy(v){busy=v;},set blocked(v){blocked=v;},set failFile(v){failFile=v;},set failCache(v){failCache=v;},set failState(v){failState=v;},set failDestination(v){failDestination=v;},set found(v){found=v;}};
}
test('normal connection prepares every asset, then navigates once with the same credential and route',async()=>{
 const f=fixture(),before=f.store.get('krill-mobile-v2');f.api.start();await f.api.check();assert.equal(f.redirects.length,0);assert.equal(f.store.get('krill-mobile-v2'),before);
 assert.equal(f.cache.size,inventory.length+1);assert(f.requests.every(r=>r.options.cache==='no-store'));
 await f.advance(1500);assert.deepEqual(f.redirects,[`/m/${f.hash}/?session=7#/host/local/group/project/s/7`]);
 const saved=JSON.parse(f.store.get('krill-mobile-v2'));assert.equal(saved.credential,'c'.repeat(64));assert.equal(saved.device_id,'device-A');assert.equal(saved.hash,f.hash);
 const handoff=JSON.parse(f.session.get('krill-mobile-update-v1'));assert.deepEqual(handoff.state,f.state);assert(!JSON.stringify(handoff).includes(saved.credential));assert(!f.requests.some(r=>r.options.method==='POST'));
 await f.advance(60000);assert.equal(f.redirects.length,1);f.api.stop();
});
test('composition, pending work and native file picker defer navigation until a quiet safe point',async()=>{
 const f=fixture();f.api.start();await f.api.check();f.doc.dispatchEvent(new Event('compositionstart'));await f.advance(2000);assert.equal(f.redirects.length,0);
 f.doc.dispatchEvent(new Event('compositionend'));f.busy=true;await f.advance(2000);assert.equal(f.redirects.length,0);
 f.busy=false;const picker=new EventTarget();picker.click=()=>{};f.context.KrillMobile.update=f.api;f.context.KrillMobile.updates.pickFile(picker);await f.advance(2000);assert.equal(f.redirects.length,0);
 picker.dispatchEvent(new Event('cancel'));await f.advance(2000);assert.equal(f.redirects.length,1);f.api.stop();
});
test('navigation waits until a service worker actually controls the page',async()=>{
 const f=fixture();f.context.navigator.serviceWorker.controller=null;f.api.start();await f.api.check();await f.advance(2000);assert.equal(f.redirects.length,0);f.context.navigator.serviceWorker.controller={};f.context.navigator.serviceWorker.dispatchEvent(new Event('controllerchange'));await f.api.check();await f.advance(1000);assert.equal(f.redirects.length,1);f.api.stop();
});
for(const failure of ['missing','cache','handoff','credential','hash','schema'])test(`${failure} failure preserves the current app and credential`,async()=>{
 const f=fixture(failure==='schema'?{change:{'index.html':read('index.html').replace('name="krill-update"','name="unsupported"')}}:{});
 const before=f.store.get('krill-mobile-v2');if(failure==='missing')f.failFile='terminal.js';if(failure==='cache')f.failCache=true;if(failure==='handoff')f.failState=true;if(failure==='credential')f.failDestination=true;if(failure==='hash')f.found={hash:'b'.repeat(16),relay:true,url:''};
 f.api.start();await f.api.check();await f.advance(2000);assert.equal(f.redirects.length,0);assert.equal(f.store.get('krill-mobile-v2'),before);f.api.stop();
});
test('restoration is scoped to host/device/hash, expires, and consumes text only once',async()=>{
 for(const mismatch of ['none','host','hash','expired','malformed']){
  const f=fixture(),record={schema:1,from:'b'.repeat(16),to:oldHash,identity:f.endpoint.updateIdentity(),at:100000,state:f.state};
  if(mismatch==='host')record.identity='another device';if(mismatch==='hash')record.to='f'.repeat(16);if(mismatch==='expired')record.at=-300000;
  f.session.set('krill-mobile-update-v1',mismatch==='malformed'?'{':JSON.stringify(record));f.api.restore();assert.equal(!!f.restored,mismatch==='none');assert.equal(f.session.size,0);f.api.restore();
 }
});
test('background suppresses polling, foreground/online coalesce discovery, auth block suppresses updates',async()=>{
 const f=fixture();f.doc.hidden=true;f.api.start();await f.api.check();assert.equal(f.requests.length,0);await f.advance(30000);assert.equal(f.requests.length,0);
 f.doc.hidden=false;f.blocked=true;f.events.dispatchEvent(new Event('online'));await flush();assert.equal(f.requests.length,0);
 f.blocked=false;f.doc.dispatchEvent(new Event('visibilitychange'));f.events.dispatchEvent(new Event('pageshow'));f.events.dispatchEvent(new Event('online'));await f.api.check();assert.equal(f.requests.filter(r=>r.url.startsWith('/r/')).length,1);f.api.stop();
});
test('host changes invalidate prepared candidates; another tab cannot be overwritten',async()=>{
 const f=fixture();f.api.start();await f.api.check();f.api.invalidate();await f.advance(2000);assert.equal(f.redirects.length,0);
 await f.api.check();const book=JSON.parse(f.store.get('krill-mobile-hosts-v1'));book.active='other';f.store.set('krill-mobile-hosts-v1',JSON.stringify(book));await f.advance(2000);assert.equal(f.redirects.length,0);f.api.stop();
});
test('a changed or unavailable host version cancels a prepared transition',async()=>{
 const f=fixture();f.api.start();await f.api.check();f.found={hash:oldHash,relay:true,url:''};await f.advance(2000);assert.equal(f.redirects.length,0);f.api.stop();
});
test('new pairings from another tab survive an update of the still-active desktop',async()=>{
 const f=fixture();f.api.start();await f.api.check();const book=JSON.parse(f.store.get('krill-mobile-hosts-v1'));
 book.hosts.push({id:'relay:other',device_id:'device-B',credential:'other-private-credential',rid:'d'.repeat(32),fixed:true});
 f.store.set('krill-mobile-hosts-v1',JSON.stringify(book));await f.advance(2000);assert.equal(f.redirects.length,1);
 assert.deepEqual(JSON.parse(f.store.get('krill-mobile-hosts-v1')).hosts.find(h=>h.device_id==='device-B'),book.hosts[1]);f.api.stop();
});
test('a deleted or re-paired active desktop is not resurrected from stale memory',async()=>{
 for(const changed of ['deleted','re-paired']){const f=fixture();f.api.start();await f.api.check();const book=JSON.parse(f.store.get('krill-mobile-hosts-v1'));if(changed==='deleted')book.hosts=[];else book.hosts[0].credential='rotated';f.store.set('krill-mobile-hosts-v1',JSON.stringify(book));await f.advance(2000);assert.equal(f.redirects.length,0);assert.equal(f.store.get('krill-mobile-hosts-v1'),JSON.stringify(book));f.api.stop();}
});
test('hash-invalid cache-first responses are evicted so a repaired deployment can update',async()=>{
 const f=fixture({swCache:true});f.cache.set(`https://phone.test/m/${f.hash}/terminal.js`,new Response('corrupt but 200'));
 f.api.start();await f.api.check();assert.equal(f.cache.size,0);assert.equal(f.redirects.length,0);await f.api.check();await f.advance(2000);assert.equal(f.redirects.length,1);f.api.stop();
});
test('legacy connection failure discovery delegates to the updater without writing a new hash',async()=>{
 const f=fixture();let notifications=0;f.endpoint.setUpdateHandler(()=>notifications++);await f.endpoint.locate();assert.equal(notifications,1);assert.equal(f.redirects.length,0);assert.equal(JSON.parse(f.store.get('krill-mobile-v2')).hash,oldHash);
});
test('view handoff restores scroll offsets without replaying any interaction',()=>{
 const f=fixture(),node={scrollTop:35,scrollLeft:20,scrollHeight:500,clientHeight:100,scrollWidth:400,clientWidth:100},root=Object.assign(new EventTarget(),{querySelectorAll:s=>s==='.chat-feed'?[node]:[]});let disconnected=false;
 f.context.MutationObserver=class{observe(){}disconnect(){disconnected=true;}};
 const saved=f.context.KrillMobile.updates.captureView(root);node.scrollTop=node.scrollLeft=0;f.context.KrillMobile.updates.restoreView(root,saved);assert.equal(node.scrollTop,35);assert.equal(node.scrollLeft,20);assert.equal(disconnected,true);assert.equal(f.requests.length,0);
});
test('a photo prepared for a previous desktop cannot send to its replacement',async()=>{
 const line=read('app.js').split('\n').find(l=>l.includes('async function photo(session,'));let decode;const decoded=new Promise(done=>decode=done),calls=[];
 const A={rpc:r=>calls.push(['A',r])},B={rpc:r=>calls.push(['B',r])};let holds=0;
 const context=vm.createContext({transport:A,hostGeneration:1,M:{update:{hold(){holds++;return ()=>holds--;}}},URL:{createObjectURL:()=>'',revokeObjectURL(){}},Image:class{decode(){return decoded;}},document:{createElement:()=>({getContext:()=>({drawImage(){}}),toBlob:done=>done({size:1,arrayBuffer:async()=>new Uint8Array([1]).buffer})})},Uint8Array,btoa:()=>'',toast(){}});
 vm.runInContext(line+';globalThis.testPhoto=photo;',context);const pending=context.testPhoto(7,{type:'image/jpeg',size:1});assert.equal(holds,1);context.transport=B;context.hostGeneration=2;decode();await assert.rejects(pending,/target changed/);assert.equal(holds,0);assert.deepEqual(calls,[]);
});
test('closing a sheet discards hidden pairing inputs so secrets cannot keep safe updates busy',()=>{
 const line=read('app.js').split('\n').find(l=>l.includes('function closeSheet()'));let cleared=0,restored=0;
 const dialog={open:true,close(){this.open=false;}},sheetBody={replaceChildren(){assert.equal(dialog.open,false);cleared++;}},context=vm.createContext({dialog,sheetBody,restoreFocus(){restored++;}});
 vm.runInContext(line+';globalThis.close=closeSheet;',context);context.close();assert.equal(cleared,1);assert.equal(restored,1);context.close();assert.equal(cleared,2);
});
