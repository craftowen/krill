// Deterministic transport contracts; no daemon, phone, network or real clock wait.
'use strict';
const test=require('node:test'),assert=require('node:assert/strict'),vm=require('node:vm'),fs=require('node:fs'),path=require('node:path');
const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/client.js'),'utf8');
const enc=new TextEncoder(),flush=async()=>{for(let i=0;i<30;i++)await Promise.resolve();};
const deferred=()=>{let resolve;const promise=new Promise(done=>resolve=done);return {promise,resolve};};
function harness(saved=null,href='https://phone.test/m/0123456789abcdef/',useEndpoint=false){
 let now=100000,nextTimer=0;const timers=new Map(),sockets=[],events=[],redirects=[],store=new Map(saved?[['krill-mobile-v2',JSON.stringify(saved)]]:[]),url=new URL(href);
 url.replace=href=>redirects.push(href);
 class Socket{constructor(url){this.url=url;this.handlers={};this.sent=[];this.readyState=0;sockets.push(this);}addEventListener(name,fn){(this.handlers[name]||=[]).push(fn);}removeEventListener(name,fn){this.handlers[name]=(this.handlers[name]||[]).filter(v=>v!==fn);}emit(name,event={}){if(name==='open')this.readyState=1;for(const fn of [...this.handlers[name]||[]])fn(event);}send(data){this.sent.push(data);}close(){this.readyState=3;this.closed=true;}}
 const C={enc,concat:(...bytes)=>Uint8Array.from(bytes.flatMap(b=>Array.from(b))),unhex:()=>new Uint8Array(32),unbase64:()=>new Uint8Array(32),hex:b=>Buffer.from(b).toString('hex'),sha256:b=>new Uint8Array(require('node:crypto').createHash('sha256').update(b).digest()),Channel:class{seal(b){return b;}open(b){return b;}}};
 const context=vm.createContext({TextEncoder,TextDecoder,Uint8Array,Uint32Array,DataView,URL,URLSearchParams,AbortController,MkCrypto:C,WebSocket:Socket,Date:class extends Date{static now(){return now;}},crypto:{getRandomValues:b=>b},location:url,history:{replaceState(){}},navigator:{},localStorage:{getItem:key=>store.get(key)||null,setItem:(key,value)=>store.set(key,value),removeItem:key=>store.delete(key)},setTimeout:(fn,ms)=>{const id=++nextTimer;timers.set(id,{fn,at:now+ms});return id;},clearTimeout:id=>timers.delete(id)});
 vm.runInContext(source,context);
 const endpoint=useEndpoint?context.KrillMobile.transport.bootstrap():null;
 const transport=context.KrillMobile.transport.create({credentials:endpoint?.credentials||{device_id:'aa',credential:'bb'},endpoint,url:'wss://desktop.test/ws',onStatus:s=>events.push(s)});
 async function tick(ms){now+=ms;for(const [id,timer] of [...timers])if(timer.at<=now){timers.delete(id);timer.fn();}await flush();}
 async function advance(socket,stage=4){if(stage>=1){socket.emit('open');await flush();}if(stage>=2){socket.emit('message',{data:JSON.stringify({version:2,challenge:'aa'})});await flush();}if(stage>=3){socket.emit('message',{data:enc.encode('{}')});await flush();}if(stage>=4){socket.emit('message',{data:Uint8Array.from([0,0,0,0,1])});await flush();}}
 async function start(stage=0){const pending=transport.connect();await flush();const socket=sockets.at(-1);await advance(socket,stage);return {pending,socket};}
 function response(socket,data,ok=true){const bytes=enc.encode(JSON.stringify(ok?{ok:true,data}:{ok:false,error:'request rejected'})),length=[];let n=bytes.length;do{const b=n&127;n>>>=7;length.push(b|(n?128:0));}while(n);socket.emit('message',{data:Uint8Array.from([0,0,0,0,0x65,6,...length,...bytes])});}
 return {context,transport,endpoint,tick,start,advance,response,sockets,events,store,redirects};
}
for(const [stage,label] of ['open','challenge','authentication','server hello','snapshot'].entries())test(`total connect deadline covers stalled ${label}`,async()=>{const h=harness(),{pending}=await h.start(stage);await h.tick(20000);await pending;assert.equal(h.transport.ready,false);assert.match(h.events.find(e=>e.message.includes('timed out'))?.message||'',/timed out/);h.transport.close();});
test('RPC total deadline breaks serial queue and never resends a mutation',async()=>{const h=harness(),{pending,socket}=await h.start(4);h.response(socket,{sessions:[]});await pending;assert.equal(h.transport.ready,true);const before=socket.sent.length,first=h.transport.rpc({action:'input',text:'once'}),second=h.transport.rpc({action:'snapshot'});first.catch(()=>{});second.catch(()=>{});await flush();assert.equal(socket.sent.length,before+1);await h.tick(15000);await assert.rejects(first,/timed out/);await assert.rejects(second);assert.equal(h.transport.ready,false);assert.equal(socket.sent.length,before+1);h.transport.close();});
test('updates wait for queued RPCs and uncertain mutation results until explicit reconnect',async()=>{
 const h=harness(),{pending,socket}=await h.start(4);h.response(socket,{sessions:[]});await pending;assert.equal(h.transport.updateBusy,false);
 const read=h.transport.rpc({action:'snapshot'}),mutation=h.transport.rpc({action:'prompt',text:'once'});mutation.catch(()=>{});assert.equal(h.transport.updateBusy,true);await flush();h.response(socket,{sessions:[]});await read;await flush();assert.equal(h.transport.updateBusy,true);
 await h.tick(15000);await assert.rejects(mutation,/timed out/);assert.equal(h.transport.updateBusy,true);const count=socket.sent.length;h.transport.resume(true);await flush();await h.advance(h.sockets.at(-1));h.response(h.sockets.at(-1),{sessions:[]});await flush();assert.equal(h.transport.updateBusy,true);h.transport.reconnect();await flush();assert.equal(socket.sent.length,count);await h.advance(h.sockets.at(-1));h.response(h.sockets.at(-1),{sessions:[]});await flush();assert.equal(h.transport.updateBusy,false);h.transport.close();
});
test('a definitive daemon rejection does not leave automatic updates blocked',async()=>{const h=harness(),{pending,socket}=await h.start(4);h.response(socket,{sessions:[]});await pending;const rejected=h.transport.rpc({action:'prompt',text:'rejected'});await flush();h.response(socket,{},false);await assert.rejects(rejected,/request rejected/);assert.equal(h.transport.updateBusy,false);h.transport.close();});
for(const stage of [0,1])test(`version mismatch at handshake stage ${stage} pauses retries for a safe update`,async()=>{
 const h=harness({device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32),hash:'0123456789abcdef'},undefined,true);let checks=0;h.endpoint.setUpdateHandler(()=>checks++);h.context.fetch=async()=>({ok:true,json:async()=>({hash:'fedcba9876543210',relay:true,url:''})});
 const {pending,socket}=await h.start(stage);socket.emit('close',{code:1012});await pending;assert.equal(checks,1);assert.equal(h.endpoint.updatePending,true);assert.equal(h.transport.updateBusy,false);assert.equal(h.transport.state.retryAt,0);await h.tick(30000);assert.equal(h.sockets.length,1);h.endpoint.clearUpdatePending();h.transport.resume();await flush();assert.equal(h.sockets.length,2);h.transport.close();
});
test('replacement cancellation restores saved destination and credential together',()=>{const saved={device_id:'a'.repeat(64),credential:'b'.repeat(64),rid:'c'.repeat(32),url:'https://saved.trycloudflare.com'},h=harness(saved,'https://phone.test/m/0123456789abcdef/#pair=aa&key=bb&r='+'d'.repeat(32)),endpoint=h.context.KrillMobile.transport.bootstrap();assert.equal(endpoint.replacing,true);assert.match(endpoint.socketUrl(),/\/r\/dddd/);const restored=endpoint.restoreSaved();assert.equal(restored.device_id,saved.device_id);assert.equal(endpoint.socketUrl(),'wss://saved.trycloudflare.com/ws?v=0123456789abcdef');assert.equal(JSON.parse(h.store.get('krill-mobile-v2')).rid,saved.rid);});
test('device capacity is blocked with cleanup guidance rather than expired QR diagnosis',async()=>{const h=harness(),{pending,socket}=await h.start(2);socket.emit('close',{code:4009});await pending;assert.equal(h.transport.blocked,true);assert.match(h.transport.state.message,/device limit/);assert.doesNotMatch(h.transport.state.message,/expired/);h.transport.close();});
test('cooldown exposes retryAt and explicit reconnect preserves server backoff',async()=>{const h=harness(),{pending,socket}=await h.start(2);socket.emit('close',{code:1013});await pending;const retryAt=h.transport.state.retryAt;assert.ok(retryAt>100000);assert.equal(h.transport.blocked,false);h.transport.reconnect();await flush();assert.equal(h.sockets.length,1);assert.ok(h.transport.state.retryAt>=retryAt);h.transport.close();});
test('fresh pairing can recover a blocked transport',async()=>{const h=harness(),{pending,socket}=await h.start(2);socket.emit('close',{code:1008});await pending;assert.equal(h.transport.blocked,true);h.transport.setPair({ticket:'new',secret:'secret'});await flush();assert.equal(h.transport.blocked,false);assert.equal(h.sockets.length,2);h.transport.close();});
test('warm terminal negotiates TERM_MODES with the daemon',async()=>{const h=harness(),{pending,socket}=await h.start(4);h.response(socket,{sessions:[]});await pending;const warm=h.sockets.at(-1);assert.notEqual(warm,socket);warm.emit('open');warm.emit('message',{data:JSON.stringify({version:2,challenge:'aa'})});const bytes=warm.sent[2];let at=5;const read=()=>{let n=0n,shift=0n,b;do{b=bytes[at++];n|=BigInt(b&127)<<shift;shift+=7n;}while(b&128);return n;};assert.equal(read(),1n);assert.equal(read(),1n);assert.ok((read()&8192n)!==0n);h.transport.close();});
test('retry during authentication cancels old generation and starts a new attempt',async()=>{const h=harness(),{pending,socket}=await h.start(2);h.transport.reconnect();await flush();await pending;assert.equal(h.transport.blocked,false);assert.equal(h.sockets.length,2);assert.equal(socket.closed,true);assert.equal(h.transport.state.phase,'connecting');h.transport.close();});
test('late open from expired connection cannot consume or send on the next attempt',async()=>{const h=harness(),{pending,socket}=await h.start(0);await h.tick(20000);await pending;h.transport.reconnect();await flush();const next=h.sockets.at(-1);socket.emit('open');await flush();assert.equal(next.sent.length,0);assert.equal(socket.sent.length,0);assert.equal(h.transport.state.phase,'connecting');h.transport.close();});
test('replacement cancellation returns to saved app version when QR was for another build',()=>{const saved={device_id:'a'.repeat(64),credential:'b'.repeat(64),rid:'c'.repeat(32),hash:'fedcba9876543210'},h=harness(saved,'https://phone.test/m/0123456789abcdef/#pair=aa&key=bb&r='+'d'.repeat(32)),endpoint=h.context.KrillMobile.transport.bootstrap();assert.equal(endpoint.restoreSaved().hash,saved.hash);assert.deepEqual(h.redirects,['/m/fedcba9876543210/']);});
for(const located of [false,true])for(const phase of ['connecting','ready'])test(`late locate(${located}) cannot disconnect or stop a ${phase} replacement connection`,async()=>{
 const h=harness({device_id:'aa',credential:'bb',rid:'c'.repeat(32)},undefined,true),lookup=deferred();let ownership,fallbacks=0;
 h.endpoint.locate=options=>{ownership=options;return lookup.promise;};h.endpoint.fallback=()=>fallbacks++;
 const {pending,socket}=await h.start();socket.emit('error');await flush();
 h.transport.reconnect();await flush();const replacement=h.sockets.at(-1);assert.notEqual(replacement,socket);
 if(phase==='ready'){await h.advance(replacement);h.response(replacement,{sessions:[]});await flush();assert.equal(h.transport.ready,true);}
 const events=h.events.length;lookup.resolve(located);await pending;await flush();assert.equal(replacement.closed,undefined);assert.equal(h.transport.ready,phase==='ready');assert.equal(h.transport.state.phase,phase);assert.equal(h.events.length,events);assert.equal(fallbacks,0);assert.equal(ownership.isCurrent(),false);assert.equal(ownership.signal.aborted,true);
 if(phase==='connecting'){await h.advance(replacement);h.response(replacement,{sessions:[]});await flush();assert.equal(h.transport.ready,true);}
 h.transport.reconnect();await flush();const next=h.sockets.at(-1);assert.notEqual(next,replacement);await h.advance(next);h.response(next,{sessions:[]});await flush();assert.equal(h.transport.ready,true);h.transport.close();
});
for(const delayed of ['fetch','json'])test(`stale discovery ${delayed} success cannot change routing, credentials or location`,async()=>{
 const saved={device_id:'a'.repeat(64),credential:'b'.repeat(64),rid:'c'.repeat(32),url:'https://saved.trycloudflare.com',hash:'0123456789abcdef'},h=harness(saved,undefined,true),fetchResult=deferred(),jsonResult=deferred();let signal,jsonReads=0;
 const response={ok:true,json(){jsonReads++;return jsonResult.promise;}};h.context.fetch=(_url,options)=>{signal=options.signal;return fetchResult.promise;};
 const {pending,socket}=await h.start();socket.emit('error');await flush();assert.equal(signal.aborted,false);
 if(delayed==='json'){fetchResult.resolve(response);await flush();assert.equal(jsonReads,1);}
 h.transport.reconnect();await flush();const replacement=h.sockets.at(-1);assert.notEqual(replacement,socket);
 await h.advance(replacement);h.response(replacement,{sessions:[]});await flush();assert.equal(h.transport.ready,true);
 const routing=h.endpoint.socketUrl(),stored=h.store.get('krill-mobile-v2'),credentials=JSON.stringify(h.transport.credentials),endpointCredentials=JSON.stringify(h.endpoint.authenticated({})),events=h.events.length;
 jsonResult.resolve({hash:'fedcba9876543210',url:'https://stale.trycloudflare.com',relay:false});fetchResult.resolve(response);await pending;await flush();
 assert.equal(h.endpoint.socketUrl(),routing);assert.equal(h.store.get('krill-mobile-v2'),stored);assert.equal(JSON.stringify(h.transport.credentials),credentials);assert.equal(JSON.stringify(h.endpoint.authenticated({})),endpointCredentials);assert.deepEqual(h.redirects,[]);assert.equal(jsonReads,delayed==='json'?1:0);assert.equal(signal.aborted,true);assert.equal(replacement.closed,undefined);assert.equal(h.transport.ready,true);assert.equal(h.transport.state.phase,'ready');assert.equal(h.events.length,events);h.transport.close();
});
test('closing a connection cancels pending discovery before a late JSON redirect',async()=>{
 const saved={device_id:'a'.repeat(64),credential:'b'.repeat(64),rid:'c'.repeat(32),hash:'0123456789abcdef'},h=harness(saved,undefined,true),jsonResult=deferred();let signal;
 h.context.fetch=async(_url,options)=>{signal=options.signal;return {ok:true,json:()=>jsonResult.promise};};const {pending,socket}=await h.start();socket.emit('error');await flush();const stored=h.store.get('krill-mobile-v2');h.transport.close();jsonResult.resolve({hash:'fedcba9876543210',url:'https://stale.trycloudflare.com'});await pending;assert.equal(h.store.get('krill-mobile-v2'),stored);assert.deepEqual(h.redirects,[]);assert.equal(signal.aborted,true);assert.equal(h.transport.ready,false);
});

test('pairing a second desktop keeps the first credential and switching restores its destination',()=>{
 const saved={device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32),url:'https://first.trycloudflare.com',hash:'0123456789abcdef'},h=harness(saved,undefined,true),e=h.endpoint;
 assert.equal(e.profiles().length,1);const first=e.profiles()[0].id;e.describeHost({display_name:'Home PC',platform:'macos'});
 e.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'b'.repeat(32)).use();e.authenticated({device_id:'second',credential:'22'.repeat(32)});e.describeHost({display_name:'Work PC',platform:'windows'});
 assert.equal(e.profiles().length,2);const second=e.profiles().find(h=>h.active).id;assert.notEqual(first,second);assert.match(e.socketUrl(),/\/r\/bbbb/);
 assert.equal(e.selectHost(first).device_id,'first');assert.equal(e.socketUrl(),'wss://first.trycloudflare.com/ws?v=0123456789abcdef');assert.equal(e.profiles().find(h=>h.active).name,'Home PC');
 assert.equal(e.selectHost(second).credential,'22'.repeat(32));assert.match(e.socketUrl(),/\/r\/bbbb/);assert(!JSON.stringify(e.profiles()).includes('credential'));
 const reloaded=h.context.KrillMobile.transport.bootstrap();assert.equal(reloaded.credentials.device_id,'second');assert.equal(reloaded.profiles().length,2);assert.equal(reloaded.selectHost(first).credential,saved.credential);
});
test('re-pairing the same relay desktop rotates its credential without adding a duplicate',()=>{
 const h=harness({device_id:'old',credential:'11'.repeat(32),rid:'a'.repeat(32)},undefined,true),e=h.endpoint;
 e.describeHost({display_name:'Same PC'});e.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'a'.repeat(32)).use();e.authenticated({device_id:'new',credential:'33'.repeat(32)});
 assert.equal(e.profiles().length,1);assert.equal(e.profiles()[0].name,'Same PC');assert.equal(JSON.parse(h.store.get('krill-mobile-v2')).device_id,'new');
});
test('desktop aliases survive snapshots, reconnects, reloads and re-pairing without changing credentials',()=>{
 const saved={device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32)},h=harness(saved,undefined,true),e=h.endpoint,id=e.profiles()[0].id;
 e.describeHost({display_name:'Original hostname'});e.renameHost(id,'  작업용 맥  ');e.describeHost({display_name:'Another hostname'});e.authenticated({});
 assert.equal(e.profiles()[0].name,'작업용 맥');assert.equal(e.credentials.credential,saved.credential);assert.equal(e.credentials.device_id,saved.device_id);
 const reloaded=h.context.KrillMobile.transport.bootstrap();assert.equal(reloaded.profiles()[0].name,'작업용 맥');
 reloaded.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'a'.repeat(32)).use();reloaded.authenticated({device_id:'rotated',credential:'22'.repeat(32)});reloaded.describeHost({display_name:'System hostname'});
 assert.equal(reloaded.profiles().length,1);assert.equal(reloaded.profiles()[0].name,'작업용 맥');
});
test('renaming an inactive desktop preserves the active host and failed saves preserve its old name',()=>{
 const h=harness({device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32)},undefined,true),e=h.endpoint,first=e.profiles()[0].id;
 e.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'b'.repeat(32)).use();e.authenticated({device_id:'second',credential:'22'.repeat(32)});const before=h.store.get('krill-mobile-v2');
 e.renameHost(first,'Home PC');assert.equal(e.profiles().find(h=>h.id===first).name,'Home PC');assert.equal(h.store.get('krill-mobile-v2'),before);
 for(const invalid of ['', '  ', 'x'.repeat(81)])assert.throws(()=>e.renameHost(first,invalid),/desktop name/);
 h.context.localStorage.setItem=()=>{throw Error('quota');};assert.throws(()=>e.renameHost(first,'Lost edit'),/Could not save/);assert.equal(e.profiles().find(h=>h.id===first).name,'Home PC');assert.equal(e.profiles().find(h=>h.active).id,'relay:'+'b'.repeat(32));assert.equal(h.store.get('krill-mobile-v2'),before);
});
test('uncompleted pairing never changes the persisted active desktop',()=>{
 const h=harness({device_id:'original',credential:'11'.repeat(32),rid:'a'.repeat(32)},undefined,true),e=h.endpoint,before=h.store.get('krill-mobile-hosts-v1');
 e.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'b'.repeat(32)).use();assert.equal(h.store.get('krill-mobile-hosts-v1'),before);assert.equal(e.profiles().length,1);
 const restored=e.restoreSaved();assert.equal(restored.device_id,'original');assert.match(e.socketUrl(),/\/r\/aaaa/);
});
test('forgetting an inactive desktop leaves the active authentication intact',()=>{
 const h=harness({device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32)},undefined,true),e=h.endpoint,first=e.profiles()[0].id;
 e.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'b'.repeat(32)).use();e.authenticated({device_id:'second',credential:'22'.repeat(32)});const active=e.profiles().find(h=>h.active).id;
 e.forgetHost(first);assert.equal(e.profiles().length,1);assert.equal(e.profiles()[0].id,active);assert.throws(()=>e.selectHost(first),/no longer saved/);assert.equal(JSON.parse(h.store.get('krill-mobile-v2')).device_id,'second');
});
test('disconnecting the active or last desktop removes persisted credentials without reviving it on reload',()=>{
 const h=harness({device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32)},undefined,true),e=h.endpoint,first=e.profiles()[0].id;
 e.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'b'.repeat(32)).use();e.authenticated({device_id:'second',credential:'22'.repeat(32)});const active=e.profiles().find(h=>h.active).id,stale=e.forHost(active);
 assert.equal(e.forgetHost(active),first);assert.equal(e.profiles().length,1);assert.equal(e.profiles()[0].active,true);assert.equal(e.restoreSaved().device_id,'first');assert.equal(JSON.parse(h.store.get('krill-mobile-v2')).device_id,'first');stale.describeHost({display_name:'stale'});stale.authenticated({});assert.equal(e.profiles().length,1);
 assert.equal(e.forgetHost(first),null);assert.equal(e.profiles().length,0);assert.equal(e.restoreSaved(),null);assert.equal(h.store.has('krill-mobile-v2'),false);const reloaded=h.context.KrillMobile.transport.bootstrap();assert.equal(reloaded.profiles().length,0);assert.equal(reloaded.credentials,null);h.transport.close();
});
test('a failed disconnect preserves the desktop and credentials and reports the storage error',()=>{
 const h=harness({device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32)},undefined,true),e=h.endpoint,id=e.profiles()[0].id,before=h.store.get('krill-mobile-v2');h.context.localStorage.setItem=()=>{throw Error('quota');};assert.throws(()=>e.forgetHost(id),/Could not remove/);assert.equal(e.profiles()[0].active,true);assert.equal(e.restoreSaved().device_id,'first');assert.equal(h.store.get('krill-mobile-v2'),before);h.transport.close();
});
test('another tab cannot revive a removed pairing through snapshot, authentication or common persistence',async()=>{
 const h=harness({device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32)},undefined,true),a=h.endpoint,b=h.context.KrillMobile.transport.bootstrap(),id=a.profiles()[0].id,peer=b.forHost(id);a.forgetHost(id);peer.describeHost({display_name:'stale'});peer.authenticated({});b.describeHost({display_name:'stale root'});assert.throws(()=>b.selectHost(id),/no longer saved/);h.context.fetch=async()=>({ok:true,json:async()=>({hash:'0123456789abcdef',relay:true,url:''})});await peer.locate();b.renameHost(id,'Stale tab rename');assert.equal(h.store.has('krill-mobile-v2'),false);assert.equal(JSON.parse(h.store.get('krill-mobile-hosts-v1')).hosts.length,0);assert.equal(h.context.KrillMobile.transport.bootstrap().profiles().length,0);assert.equal(b.reloadHosts(),true);assert.equal(b.profiles().length,0);
 // A new explicit QR pairing is allowed; the old peer still cannot save the removed device.
 b.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'a'.repeat(32)).use();b.authenticated({device_id:'new-device',credential:'33'.repeat(32)});peer.describeHost({display_name:'stale'});assert.equal(b.profiles().length,1);assert.equal(JSON.parse(h.store.get('krill-mobile-v2')).device_id,'new-device');h.transport.close();
});
test('switching while discovery is pending aborts the old route update',async()=>{
 const h=harness({device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32)},undefined,true),e=h.endpoint,first=e.profiles()[0].id;
 e.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'b'.repeat(32)).use();const second=e.authenticated({device_id:'second',credential:'22'.repeat(32)});e.selectHost(first);
 const transport=h.context.KrillMobile.transport.create({credentials:e.selectHost(first),endpoint:e}),lookup=deferred();h.context.fetch=()=>lookup.promise;const pending=transport.connect();await flush();h.sockets.at(-1).emit('error');await flush();transport.close();e.selectHost(second.id);const route=e.socketUrl();lookup.resolve({ok:true,json:async()=>({hash:'fedcba9876543210',url:'https://stale.trycloudflare.com'})});await pending;
 assert.equal(e.socketUrl(),route);assert.deepEqual(h.redirects,[]);assert.equal(JSON.parse(h.store.get('krill-mobile-v2')).device_id,'second');
});

test('unavailable storage keeps pairing and host selection usable in memory',()=>{
 const h=harness({device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32)},undefined,true),e=h.endpoint,first=e.profiles()[0].id;
 h.context.localStorage.setItem=()=>{throw Error('Storage disabled');};e.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'b'.repeat(32)).use();
 assert.equal(e.authenticated({device_id:'second',credential:'22'.repeat(32)}).device_id,'second');assert.equal(e.storageFailed,true);assert.equal(e.profiles().length,2);assert.equal(e.selectHost(first).device_id,'first');assert.match(e.socketUrl(),/\/r\/aaaa/);
});
test('pool keeps two desktops authenticated concurrently and isolates equal session IDs',async()=>{
 const h=harness({device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32),hash:'0123456789abcdef'},undefined,true),e=h.endpoint,first=e.profiles()[0].id;
 e.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'b'.repeat(32)).use();const second=e.authenticated({device_id:'second',credential:'22'.repeat(32)}).id;e.selectHost(first);
 const changes=[],pool=h.context.KrillMobile.transport.pool({endpoint:e,onChange:entry=>changes.push(entry.id)});pool.sync();await flush();assert.equal(h.sockets.length,2);
 const a=h.sockets.find(s=>s.url.includes('/r/aaaa')),b=h.sockets.find(s=>s.url.includes('/r/bbbb'));assert(a&&b);
 await h.advance(a);h.response(a,{host:{display_name:'First PC'},sessions:[{id:7,title:'first session'}]});await h.advance(b);h.response(b,{host:{display_name:'Second PC'},sessions:[{id:7,title:'second session'}]});await flush();
 assert.equal(pool.get(first).transport.ready,true);assert.equal(pool.get(second).transport.ready,true);assert.equal(pool.get(first).snapshot.sessions[0].title,'first session');assert.equal(pool.get(second).snapshot.sessions[0].title,'second session');assert.equal(h.sockets.length,2,'background connections do not warm terminal sockets');
 const auth=s=>JSON.parse(s.sent[0]);assert.equal(auth(a).device_id,'first');assert.equal(auth(b).device_id,'second');e.selectHost(second);pool.sync();assert.equal(a.closed,undefined);assert.equal(b.closed,undefined);assert.equal(h.sockets.length,2);
 a.emit('close',{code:1006});await flush();assert.equal(pool.get(first).transport.ready,false);assert.equal(pool.get(second).transport.ready,true);assert.equal(pool.get(second).snapshot.sessions[0].title,'second session');
 e.forgetHost(first);const old=pool.get(first);pool.sync();assert.equal(pool.get(first),undefined);assert.equal(old.transport.ready,false);assert.equal(pool.get(second).transport.ready,true);pool.close();h.transport.close();
});
test('background discovery/auth cannot replace the active desktop or revive removed credentials',async()=>{
 const h=harness({device_id:'first',credential:'11'.repeat(32),rid:'a'.repeat(32),hash:'0123456789abcdef'},undefined,true),e=h.endpoint,first=e.profiles()[0].id,peer=e.forHost(first);
 e.adopt('https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r='+'b'.repeat(32)).use();const second=e.authenticated({device_id:'second',credential:'22'.repeat(32)}).id;
 h.context.fetch=async()=>({ok:true,json:async()=>({hash:'fedcba9876543210',url:'https://firstnew.trycloudflare.com'})});await peer.locate();peer.authenticated({});peer.describeHost({display_name:'First renamed'});
 assert.equal(e.profiles().find(p=>p.active).id,second);assert.equal(JSON.parse(h.store.get('krill-mobile-v2')).device_id,'second');assert.equal(h.redirects.length,0);assert.match(peer.socketUrl(),/firstnew/);assert.equal(e.profiles().find(p=>p.id===first).name,'First renamed');assert.throws(()=>peer.authenticated({device_id:'second'}),/identity/);
 e.forgetHost(first);peer.describeHost({display_name:'stale'});peer.authenticated({});assert.equal(e.profiles().length,1);assert.equal(e.profiles()[0].id,second);h.transport.close();
});

test('storage notifications during a new pairing leave its endpoint and handshake intact',()=>{
 const line=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8').split('\n').find(l=>l.includes("globalThis.addEventListener('storage'"));let handler,reloads=0,syncs=0,updates=0;
 const context=vm.createContext({pairing:{ticket:'new-pair'},landing:false,endpoint:{reloadHosts(){reloads++;throw Error('pairing destination must not change');}},pool:{sync(){syncs++;}},update(){updates++;},addEventListener(name,fn){assert.equal(name,'storage');handler=fn;}});vm.runInContext(line,context);handler({key:'krill-mobile-hosts-v1'});context.pairing=null;context.landing=true;handler({key:'krill-mobile-removed-v1:old-device'});assert.equal(reloads,0);assert.equal(syncs,2);assert.equal(updates,2);
});

test('legacy credential migrates once and never overrides the canonical connection',()=>{
 const h=harness(),saved={device_id:'legacy',credential:'11'.repeat(32),rid:'a'.repeat(32)};
 h.store.set('minikrill-mobile-v2',JSON.stringify(saved));
 const first=h.context.KrillMobile.transport.bootstrap();assert.equal(first.credentials.device_id,'legacy');
 assert.equal(JSON.parse(h.store.get('krill-mobile-v2')).device_id,'legacy');assert.equal(h.store.has('minikrill-mobile-v2'),false);
 h.store.set('minikrill-mobile-v2',JSON.stringify({...saved,device_id:'stale'}));
 assert.equal(h.context.KrillMobile.transport.bootstrap().credentials.device_id,'legacy');h.transport.close();
});
test('legacy credential remains usable when migration storage is full',()=>{
 const h=harness(),saved={device_id:'legacy',credential:'11'.repeat(32),rid:'a'.repeat(32)};
 h.store.set('minikrill-mobile-v2',JSON.stringify(saved));h.context.localStorage.setItem=()=>{throw Error('quota');};
 const endpoint=h.context.KrillMobile.transport.bootstrap();assert.equal(endpoint.credentials.device_id,'legacy');
 assert.equal(h.store.has('minikrill-mobile-v2'),true);assert.equal(h.store.has('krill-mobile-v2'),false);h.transport.close();
});
