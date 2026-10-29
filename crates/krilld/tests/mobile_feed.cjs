// Change-feed transport contracts on a virtual clock: request counts, push latency, ordering and compatibility.
'use strict';
const test=require('node:test'),assert=require('node:assert/strict'),vm=require('node:vm'),fs=require('node:fs'),path=require('node:path');
const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/client.js'),'utf8');
const enc=new TextEncoder(),dec=new TextDecoder(),flush=async()=>{for(let i=0;i<30;i++)await Promise.resolve();};
const varint=n=>{const out=[];do{const b=n&127;n>>>=7;out.push(b|(n?128:0));}while(n);return out;};
const reply=json=>{const bytes=enc.encode(json);return Uint8Array.from([0,0,0,0,0x65,6,...varint(bytes.length),...bytes]);};
function harness(){
 let now=100000,nextTimer=0;const timers=new Map(),sockets=[],snapshots=[],errors=[],docHandlers=[];
 const document={hidden:false,addEventListener:(name,fn)=>name==='visibilitychange'&&docHandlers.push(fn),removeEventListener:(name,fn)=>{const i=docHandlers.indexOf(fn);if(i>=0)docHandlers.splice(i,1);}};
 class Socket{constructor(url){this.url=url;this.handlers={};this.sent=[];sockets.push(this);}addEventListener(name,fn){(this.handlers[name]||=[]).push(fn);}removeEventListener(name,fn){this.handlers[name]=(this.handlers[name]||[]).filter(v=>v!==fn);}emit(name,event={}){for(const fn of [...this.handlers[name]||[]])fn(event);}send(data){this.sent.push(data);}close(){this.closed=true;}}
 let corrupt=false;
 const C={enc,concat:(...bytes)=>Uint8Array.from(bytes.flatMap(b=>Array.from(b))),unhex:()=>new Uint8Array(32),unbase64:()=>new Uint8Array(32),hex:b=>Buffer.from(b).toString('hex'),sha256:b=>new Uint8Array(require('node:crypto').createHash('sha256').update(b).digest()),Channel:class{seal(b){return b;}open(b){if(corrupt)throw Error('bad frame');return b;}}};
 const context=vm.createContext({TextEncoder,TextDecoder,Uint8Array,Uint32Array,DataView,URL,URLSearchParams,AbortController,MkCrypto:C,WebSocket:Socket,document,Date:class extends Date{static now(){return now;}},crypto:{getRandomValues:b=>b},location:new URL('https://phone.test/'),history:{replaceState(){}},navigator:{},localStorage:{getItem:()=>null,setItem(){},removeItem(){}},setTimeout:(fn,ms)=>{const id=++nextTimer;timers.set(id,{fn,at:now+ms});return id;},clearTimeout:id=>timers.delete(id)});
 vm.runInContext(source,context);
 const M=context.KrillMobile,transport=M.transport.create({credentials:{device_id:'aa',credential:'bb'},url:'wss://desktop.test/ws',warmTerminal:false,onSnapshot:data=>snapshots.push({at:now,data}),onError:e=>errors.push(e.message)});
 // Run due timers in time order so a 60s window behaves like 60s of wall clock.
 async function tick(ms){const end=now+ms;for(;;){let due=null;for(const [id,t] of timers)if(t.at<=end&&(!due||t.at<due[1].at))due=[id,t];if(!due)break;timers.delete(due[0]);now=Math.max(now,due[1].at);due[1].fn();await flush();}now=end;await flush();}
 const requests=socket=>socket.sent.filter(b=>b instanceof Uint8Array&&b[4]===0x45).map(b=>{let at=5;while(b[at]&128)at++;at++;while(b[at]&128)at++;at++;return JSON.parse(dec.decode(b.slice(at)));});
 const respond=(socket,data,ok=true)=>socket.emit('message',{data:reply(JSON.stringify(ok?{ok:true,data}:{ok:false,error:'invalid mobile authentication or frame'}))});
 const push=(socket,data)=>socket.emit('message',{data:reply(data===undefined?'{"push":"stale"}':`{"push":"snapshot","data":${JSON.stringify(data)}}`)});
 async function connect(){const pending=transport.connect();await flush();const socket=sockets.at(-1);socket.emit('open');await flush();socket.emit('message',{data:JSON.stringify({version:2,challenge:'aa'})});await flush();socket.emit('message',{data:enc.encode('{}')});await flush();socket.emit('message',{data:Uint8Array.from([0,0,0,0,1])});await flush();return {pending,socket};}
 const visibility=hidden=>{document.hidden=hidden;for(const fn of [...docHandlers])fn();};
 return {M,transport,tick,requests,respond,push,connect,visibility,snapshots,errors,sockets,docHandlers,get now(){return now;},set corrupt(v){corrupt=v;}};
}
const body=(etag,extra={})=>({sessions:[],etag,capabilities:{change_feed:true},...extra});
// Bring a transport to the armed state against a feed-capable daemon and return the socket.
async function armed(h){
 const {pending,socket}=await h.connect();h.respond(socket,body('e1',{rev:1,feed:0}));await pending;
 await h.tick(2500);const [,second]=h.requests(socket);assert.equal(second.feed,2500);assert.equal(second.since,'e1');
 h.respond(socket,{unchanged:true,etag:'e1',rev:1,feed:2500});await flush();return socket;
}

test('legacy daemon: requests stay byte-compatible and keep the 2.5s cadence',async()=>{
 const h=harness(),{pending,socket}=await h.connect();h.respond(socket,{sessions:[],etag:'old'});await pending;
 for(let i=0;i<24;i++){await h.tick(2500);h.respond(socket,{unchanged:true,etag:'old'});await flush();}
 const sent=h.requests(socket);assert.equal(sent.length,25);
 for(const r of sent)assert.deepEqual(Object.keys(r).sort(),r.since?['action','since']:['action']);
 h.transport.close();
});

test('feed daemon: idle minute needs 2 snapshot requests instead of 24 while pushes arrive without a request',async()=>{
 const h=harness(),socket=await armed(h),before=h.requests(socket).length,rendered=h.snapshots.length;
 await h.tick(10000);h.push(socket,body('e2',{rev:2}));const pushedAt=h.now;await flush();
 assert.equal(h.snapshots.length,rendered+1,'push renders without any request');assert.equal(h.snapshots.at(-1).at,pushedAt,'zero client-side delay after the daemon push');assert.equal(h.snapshots.at(-1).data.etag,'e2');
 // Only the 30s reconciliation refresh remains; it carries the newest etag and keeps the feed armed.
 await h.tick(29999);assert.equal(h.requests(socket).length,before);await h.tick(1);
 const extra=h.requests(socket).slice(before);assert.equal(extra.length,1);assert.deepEqual(JSON.parse(JSON.stringify(extra[0])),{action:'snapshot',since:'e2',feed:2500});
 h.respond(socket,{unchanged:true,etag:'e2',rev:2,feed:2500});await flush();
 // Measured window: 60s after the push. Polling would send 24 snapshot requests (one per 2.5s).
 await h.tick(30000);assert.equal(h.requests(socket).length-before,2);h.respond(socket,{unchanged:true,etag:'e2',rev:2,feed:2500});await flush();
 assert.equal(h.errors.length,0);assert.equal(h.transport.ready,true);h.transport.close();
});

test('stale pushes and late replies never move the dashboard backwards',async()=>{
 const h=harness(),socket=await armed(h);
 h.push(socket,body('e3',{rev:3}));await flush();assert.equal(h.snapshots.at(-1).data.etag,'e3');
 const count=h.snapshots.length;h.push(socket,body('e2',{rev:2}));await flush();assert.equal(h.snapshots.length,count,'older rev ignored');
 h.push(socket,body('e3',{rev:3}));await flush();assert.equal(h.snapshots.length,count,'same etag does not re-render');
 const refresh=h.transport.refresh();await flush();h.push(socket,body('e5',{rev:5}));await flush();
 h.respond(socket,body('e4',{rev:4,feed:2500}));const result=await refresh;assert.equal(result.etag,'e5','reply computed before the newer push is ignored');assert.equal(h.snapshots.at(-1).data.etag,'e5');
 h.transport.close();
});

test('mutations are sent immediately and a push between request and reply cannot steal the reply',async()=>{
 const h=harness(),socket=await armed(h),before=socket.sent.length;
 const mutation=h.transport.rpc({action:'prompt',session:1,text:'once'});await flush();
 assert.equal(socket.sent.length,before+1,'no long watch is queued ahead of the mutation');
 h.push(socket,body('e2',{rev:2}));await flush();assert.equal(h.snapshots.at(-1).data.etag,'e2');
 h.respond(socket,{sent:true});assert.deepEqual(JSON.parse(JSON.stringify(await mutation)),{sent:true});
 assert.equal(h.transport.updateBusy,false);assert.equal(socket.sent.length,before+1,'nothing resent');h.transport.close();
});

test('hidden tab pauses the feed and sends nothing until it is visible again',async()=>{
 const h=harness(),socket=await armed(h);
 h.visibility(true);await flush();const pause=h.requests(socket).at(-1);assert.equal(pause.feed,0);
 h.respond(socket,{unchanged:true,etag:'e1',rev:1,feed:0});await flush();const count=h.requests(socket).length;
 await h.tick(120000);assert.equal(h.requests(socket).length,count,'no traffic while hidden');
 h.visibility(false);h.transport.resume(false);await flush();assert.equal(h.requests(socket).at(-1).feed,2500);
 h.respond(socket,body('e9',{rev:9,feed:2500}));await flush();assert.equal(h.snapshots.at(-1).data.etag,'e9','resume re-syncs immediately');
 h.transport.close();assert.equal(h.docHandlers.length,0,'close removes the visibility listener');
});

test('terminal focus re-arms the feed at the legacy 10s terminal cadence',async()=>{
 const h=harness(),socket=await armed(h),count=h.requests(socket).length;
 h.M.activeTerminal={};await h.tick(2500);const request=h.requests(socket).at(-1);assert.equal(h.requests(socket).length,count+1);assert.equal(request.feed,10000);
 h.respond(socket,{unchanged:true,etag:'e1',rev:1,feed:10000});await flush();await h.tick(25000);assert.equal(h.requests(socket).length,count+1);
 h.transport.close();
});

test('stale push notice falls back to one ordinary snapshot request',async()=>{
 const h=harness(),socket=await armed(h),count=h.requests(socket).length;
 h.push(socket);await flush();assert.equal(h.requests(socket).length,count+1);assert.equal(h.requests(socket).at(-1).action,'snapshot');
 h.respond(socket,body('big',{rev:4,feed:2500}));await flush();assert.equal(h.snapshots.at(-1).data.etag,'big');h.transport.close();
});

test('reconnect pipelines the remembered feed request and a downgraded daemon gets one plain retry',async()=>{
 const h=harness(),socket=await armed(h);
 socket.emit('close',{code:1006});await flush();await h.tick(0);
 const next=h.sockets.at(-1);assert.notEqual(next,socket);
 const pending=(async()=>{next.emit('open');await flush();next.emit('message',{data:JSON.stringify({version:2,challenge:'aa'})});await flush();})();await pending;
 const first=h.requests(next)[0];assert.equal(first.feed,2500,'capability remembered across reconnect');
 next.emit('message',{data:enc.encode('{}')});await flush();next.emit('message',{data:Uint8Array.from([0,0,0,0,1])});await flush();
 h.respond(next,null,false);await flush();
 const retry=h.requests(next)[1];assert.deepEqual(retry,{action:'snapshot',since:'e1'},'one plain read retry without feed');
 h.respond(next,{sessions:[],etag:'legacy'});await flush();assert.equal(h.transport.ready,true);
 assert.equal(h.requests(next).length,2);await h.tick(2500);assert.deepEqual(h.requests(next)[2],{action:'snapshot',since:'legacy'},'old daemon stays on plain polling');
 h.transport.close();
});

test('reconnect resets rev so a restarted daemon starting at rev 1 is accepted',async()=>{
 const h=harness(),socket=await armed(h);h.push(socket,body('e7',{rev:7}));await flush();
 h.transport.reconnect();await flush();const next=h.sockets.at(-1);next.emit('open');await flush();next.emit('message',{data:JSON.stringify({version:2,challenge:'aa'})});await flush();next.emit('message',{data:enc.encode('{}')});await flush();next.emit('message',{data:Uint8Array.from([0,0,0,0,1])});await flush();
 h.respond(next,body('fresh',{rev:1,feed:2500}));await flush();assert.equal(h.transport.ready,true);assert.equal(h.snapshots.at(-1).data.etag,'fresh');
 h.push(next,body('fresh2',{rev:2}));await flush();assert.equal(h.snapshots.at(-1).data.etag,'fresh2');h.transport.close();
});

test('a frame that fails decryption closes the connection instead of desynchronizing replies',async()=>{
 const h=harness(),socket=await armed(h);h.corrupt=true;h.push(socket,body('x',{rev:9}));await flush();
 assert.equal(h.transport.ready,false);assert.equal(socket.closed,true);h.transport.close();
});

test('a lost push is reconciled by the 30s refresh and the next change still pushes',async()=>{
 const h=harness(),socket=await armed(h),before=h.requests(socket).length;
 // The daemon changed to e2 but its push never arrived (e.g. dropped under relay backpressure).
 await h.tick(30000);const reconcile=h.requests(socket).slice(before);assert.equal(reconcile.length,1);assert.equal(reconcile[0].since,'e1');
 h.respond(socket,body('e2',{rev:2,feed:2500}));await flush();assert.equal(h.snapshots.at(-1).data.etag,'e2');
 h.push(socket,body('e3',{rev:3}));await flush();assert.equal(h.snapshots.at(-1).data.etag,'e3');
 assert.equal(h.requests(socket).length,before+1);h.transport.close();
});

test('pushes during a slow snapshot reply keep the newest dashboard and the reply resolves',async()=>{
 const h=harness(),socket=await armed(h);const refresh=h.transport.refresh();await flush();
 for(let r=2;r<=6;r++)h.push(socket,body('p'+r,{rev:r}));await flush();assert.equal(h.snapshots.at(-1).data.etag,'p6');
 await h.tick(14000);assert.equal(h.transport.ready,true,'pushes do not consume the pending reply slot');
 h.respond(socket,body('p6',{rev:6,feed:2500}));const result=await refresh;assert.equal(result.etag,'p6');h.transport.close();
});

test('a definitive read error after the feed is armed keeps the feed instead of downgrading to polling',async()=>{
 const h=harness(),socket=await armed(h),before=h.requests(socket).length;
 const refresh=h.transport.refresh();await flush();h.respond(socket,null,false);await assert.rejects(refresh,/invalid/);
 assert.equal(h.requests(socket).length,before+1,'no plain retry once the daemon accepted feed');
 await h.tick(25000);assert.equal(h.requests(socket).length,before+1,'still push mode: no 2.5s polling');
 await h.tick(5000);const next=h.requests(socket).at(-1);assert.equal(h.requests(socket).length,before+2);assert.equal(next.feed,2500,'reconcile keeps negotiating feed');h.respond(socket,{unchanged:true,etag:'e1',rev:1,feed:2500});await flush();
 const mutation=h.transport.rpc({action:'prompt',session:1,text:'once'});await flush();h.respond(socket,null,false);await assert.rejects(mutation,/invalid/);
 await h.tick(1000);assert.equal(h.requests(socket).filter(r=>r.action==='prompt').length,1,'mutations are never retried');
 h.transport.close();
});
