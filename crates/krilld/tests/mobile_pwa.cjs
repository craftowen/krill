/* Independent non-secure-context client against real krill-holder/krilld. */
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),os=require('node:os'),vm=require('node:vm'),crypto=require('node:crypto'),zlib=require('node:zlib');
const net=require('node:net');
const {spawn,execFileSync}=require('node:child_process'),{once}=require('node:events');
const daemonBin=path.resolve(process.argv[2]),bin=path.dirname(daemonBin),mk=path.join(bin,'krill'),assets=path.join(__dirname,'../src/mobile/assets');
const keep=process.argv.includes('--keep'),metadata=process.env.M9_BROWSER_METADATA||'/tmp/m9-browser-fixture.json';
const sandbox={TextEncoder,TextDecoder,Uint8Array,Uint32Array,DataView,BigInt,atob,crypto:{getRandomValues:bytes=>crypto.randomFillSync(bytes)},setTimeout,clearTimeout};
vm.createContext(sandbox);for(const name of ['crypto.js','terminal.js'])vm.runInContext(fs.readFileSync(path.join(assets,name),'utf8'),sandbox);const C=sandbox.MkCrypto;
assert.equal(sandbox.crypto.subtle,undefined);assert.equal(sandbox.crypto.randomUUID,undefined);
for(const n of [0,1,3,63,64,65,1024]){const bytes=crypto.randomBytes(n);assert.equal(C.hex(C.sha256(bytes)),crypto.createHash('sha256').update(bytes).digest('hex'));}
// Compare both directions to Node/OpenSSL, including block edges and a photo-sized frame.
for(const size of [0,1,15,16,17,63,64,65,1024,512*1024]){
 const secret=crypto.randomBytes(32),salt=crypto.randomBytes(64),client=new C.Channel(secret,salt),server=new C.Channel(secret,salt,false);
 for(const [sender,receiver,info,prefix] of [[client,server,'client-to-daemon','MKC2'],[server,client,'daemon-to-client','MKD2']]){
  const key=Buffer.from(crypto.hkdfSync('sha256',secret,salt,Buffer.from('mk-mobile-v2/'+info),32));
  for(let seq=0;seq<2;seq++){
   const plain=crypto.randomBytes(size),wire=Buffer.from(sender.seal(plain)),aad=Buffer.alloc(8);aad.writeBigUInt64LE(BigInt(seq));
   const cipher=crypto.createCipheriv('chacha20-poly1305',key,Buffer.concat([Buffer.from(prefix),aad]),{authTagLength:16});cipher.setAAD(aad);
   assert.deepEqual(wire,Buffer.concat([aad,cipher.update(plain),cipher.final(),cipher.getAuthTag()]));
   const tampered=Buffer.from(wire);tampered[tampered.length-1]^=1;assert.throws(()=>receiver.open(tampered),/Authentication/);
   assert.deepEqual(Buffer.from(receiver.open(wire)),plain);assert.throws(()=>receiver.open(wire),/Replay/);
  }
 }
}
const vector=new C.Channel(new Uint8Array(32).fill(7),new Uint8Array(64).fill(9));assert.equal(C.hex(vector.seal(new TextEncoder().encode('prompt'))),'0000000000000000ca8c87d414301b7fc3a6a3f09a5fb977fb9422ea97cb');
// Mobile screens without a browser: account usage values, Needs You, connection log, toast and same-tab pairing links.
for(const name of ['screens/details.js','screens/home.js','screens/settings.js'])vm.runInContext(fs.readFileSync(path.join(assets,name),'utf8'),sandbox);
{const A=sandbox.KrillMobile.accounts,now=1_000_000;
 assert.deepEqual({...A.usageWindow(null,now)},{percent:null,text:'—',reset:null});assert.equal(A.usageWindow({percent:23,resets_at:String(now+3*3600+54*60)},now).reset,'3h 54m until reset');
 assert.equal(A.usageWindow({percent:88,resets_at:String(now-1)},now).text,'Reset');assert.equal(A.usageWindow({percent:40,resets_at:new Date((now+90)*1000).toISOString()},now).reset,'1m until reset');assert.equal(A.usageWindow({percent:100,resets_at:String(now+2*86400+3600)},now).reset,'2d 1h until reset');
 const data={accounts:[],statuses:[{account_id:'w',status:'needs_login',updated_at:now-120,detail:'Run claude login'}],connections:{supported:true,accounts:[{account_id:'h',status:'not_installed'}]}};
 assert.equal(A.accountView({id:'h',provider:'Codex',label:'Host (.codex)',email:null,profile_dir:'/x',active:false,host_default:true,stale:false,usage:{five_hour:null,seven_day:null}},data,now).hidden,true);
 const v=A.accountView({id:'w',provider:'Claude',label:'Work',email:'w@example.com',profile_dir:'',active:false,host_default:false,stale:false,usage:{five_hour:{percent:5,resets_at:null},seven_day:null}},data,now);
 assert.equal(v.title,'w@example.com');assert.equal(v.switchable,false);assert.equal(v.error,'Run claude login');assert.match(v.meta,/Usage only · Sign-in required · Updated 2m ago/);assert.equal(v.windows[0][1].text,'5%');assert.equal(v.windows[1][1].text,'—');assert.equal(v.windows[1][1].percent,null);}
{const sessions=new Map([[1,{id:1,state:'Blocked'}],[2,{id:2,state:'Waiting'}],[3,{id:3,state:'Waiting',exited:1}],[4,{id:4,state:'Working'}]]),need=sandbox.KrillMobile.needsYou({sessions,snapshot:{orchestration:{gates:[{}],questions:[{},{}]}}});assert.equal(need.total,5);assert.deepEqual([...need.sessions.map(s=>s.id)],[2,1]);}
{const L=sandbox.KrillMobile.connectionLog;assert.equal(L.elapsed(1234),'+1.23s');assert.equal(L.elapsed(12345),'+12.3s');assert.equal(L.elapsed(123456),'+123s');assert.match(L.clock(0),/^\d\d:\d\d:\d\d$/);}
{const lines=fs.readFileSync(path.join(assets,'app.js'),'utf8').split('\n'),timers=[],notice=new EventTarget();notice.hidden=true;notice.textContent='';
 const context=vm.createContext({notice,t:s=>s,URLSearchParams,setTimeout:fn=>timers.push({fn}),clearTimeout:id=>{if(timers[id-1])timers[id-1].fn=()=>{};}});
 vm.runInContext(lines.find(l=>l.includes('let noticeTimer'))+lines.find(l=>l.includes('const pairingLink='))+'globalThis.T={toast,error,pairingLink};',context);const T=context.T;
 T.error(Error('RPC failed'));assert.equal(notice.hidden,false);assert.equal(notice.textContent,'RPC failed');T.error(Error('RPC failed'));timers[0].fn();assert.equal(notice.hidden,false);timers.at(-1).fn();assert.equal(notice.hidden,true);
 T.toast('Photo attached',3000);notice.dispatchEvent(new Event('click'));assert.equal(notice.hidden,true);
 assert.equal(T.pairingLink('#pair=abc&key=def'),true);assert.equal(T.pairingLink('#t=x.trycloudflare.com&r=rid'),true);assert.equal(T.pairingLink('#/host/local/group/x/s/1'),false);assert.equal(T.pairingLink('#/pair-confirm'),false);assert.equal(T.pairingLink(''),false);}
console.log('PASS mobile screens: account usage/reset/status, Needs You, connection log, toast auto-hide and same-tab pairing link detection');
for(const name of ['index.html','app.js','app.css','i18n.js','manifest.json','icon.svg','icon-192.png','icon-512.png','icon-maskable-512.png','apple-touch-icon.png','crypto.js','client.js','terminal.js','qr.js','sw.js','screens/chat.js','screens/home.js','screens/workspaces.js','screens/session.js','screens/details.js','screens/settings.js'])assert.deepEqual(zlib.gunzipSync(fs.readFileSync(path.join(assets,name+'.gz'))),fs.readFileSync(path.join(assets,name)));
const html=fs.readFileSync(path.join(assets,'index.html'),'utf8');let scriptAt=-1;for(const name of ['crypto.js','i18n.js','client.js','terminal.js','screens/home.js','screens/workspaces.js','screens/session.js','screens/details.js','screens/settings.js','app.js']){const at=html.indexOf('src="'+name+'" defer');assert(at>scriptAt,name);scriptAt=at;assert(!fs.readFileSync(path.join(assets,name),'utf8').includes('innerHTML'));}
// Fixed-origin bootstrap and transport, with real crypto and deterministic sockets.
async function fixedOriginTests(){
 const hash='0123456789abcdef',rid='a'.repeat(32),saved={device_id:'saved',credential:'ab'.repeat(32),rid,url:'https://saved.trycloudflare.com',hash};
 const source=fs.readFileSync(path.join(assets,'client.js'),'utf8');
 function fixture(href,credential=saved,found={url:'https://fresh.trycloudflare.com',hash},{serviceWorker=true}={}){
  const location=new URL(href),values=new Map(credential?[['minikrill-mobile-v2',JSON.stringify(credential)]]:[]),requests=[],registrations=[],sockets=[],timers=new Map(),redirects=[];let id=0;
  location.replace=href=>redirects.push(href);
  class Socket extends EventTarget{
   constructor(url){super();this.url=url;this.sent=[];sockets.push(this);}
   send(bytes){this.sent.push(bytes);}
   emit(type,data){const e=new Event(type);if(type==='message')e.data=data;if(type==='close')e.code=data;this.dispatchEvent(e);}
   close(){if(this.closed)return;this.closed=true;this.emit('close',1006);}
  }
  const context={AbortController,TextEncoder,TextDecoder,Uint8Array,DataView,BigInt,URL,URLSearchParams,location,history:{replaceState(a,b,url){location.href=new URL(url,location).href;}},navigator:serviceWorker?{serviceWorker:{register:url=>{registrations.push(url);return Promise.resolve();}}}:{},localStorage:{getItem:key=>values.get(key),setItem:(key,value)=>values.set(key,value)},fetch:async(url,options)=>{requests.push({url,options});return {ok:true,json:async()=>found};},setTimeout:(fn,ms)=>{timers.set(++id,{fn,ms});return id;},clearTimeout:id=>timers.delete(id),crypto:{getRandomValues:bytes=>bytes.fill(1)},WebSocket:Socket,MkCrypto:C};
  vm.createContext(context);vm.runInContext(source,context);const endpoint=context.KrillMobile.transport.bootstrap();
  return {context,endpoint,location,values,requests,registrations,sockets,timers,redirects,stored:()=>JSON.parse(values.get('minikrill-mobile-v2'))};
 }
 const flush=async()=>{for(let n=0;n<25;n++)await Promise.resolve();};
 const fixed=`https://krill-mobile.pages.dev/m/${hash}/`;
 let f=fixture(fixed+`?session=7&pair=query&key=query&t=evil&r=evil#pair=ticket&key=secret&t=paired.trycloudflare.com&r=${rid}`);
 assert.equal(f.endpoint.credentials,null);assert.equal(f.endpoint.pair.ticket,'ticket');assert.equal(f.endpoint.pair.secret,'secret');assert.equal(f.location.href,fixed+'?session=7#/pair-confirm');assert.deepEqual(f.registrations,['/sw.js']);assert.equal(f.endpoint.socketUrl(),`wss://paired.trycloudflare.com/ws?v=${hash}`);
 for(const fragment of ['#t=evil.trycloudflare.com&r=forged','#pair=only&t=evil.trycloudflare.com&r=forged','#key=only&t=evil.trycloudflare.com&r=forged']){
  f=fixture(fixed+fragment);assert.equal(f.endpoint.pair,null);assert.equal(f.endpoint.socketUrl(),`wss://saved.trycloudflare.com/ws?v=${hash}`);await f.endpoint.locate();assert.equal(f.requests[0].url,'/r/'+rid);
 }
 f=fixture(fixed+'?pair=query&key=query&t=evil.trycloudflare.com&r=forged');assert.equal(f.endpoint.pair,null);assert.equal(f.endpoint.socketUrl(),`wss://saved.trycloudflare.com/ws?v=${hash}`);assert.equal(f.location.search,'');
 // Browsers expose service workers on HTTP loopback, but not ordinary insecure LAN origins.
 f=fixture('http://127.0.0.1:1234/#pair=ticket&key=secret');assert.equal(f.endpoint.socketUrl(),'ws://127.0.0.1:1234/ws');assert.deepEqual(f.registrations,['/sw.js']);
 f=fixture('http://192.0.2.1:1234/#pair=ticket&key=secret',null,undefined,{serviceWorker:false});assert.equal(f.endpoint.socketUrl(),'ws://192.0.2.1:1234/ws');assert.deepEqual(f.registrations,[]);
 // Home Screen install link: pairs an empty store once; relaunches with the same start URL keep the saved device.
 f=fixture(fixed+`#pair=ticket&key=secret&t=paired.trycloudflare.com&r=${rid}&install=1`,null);assert.equal(f.endpoint.pair.ticket,'ticket');assert.equal(f.location.href,fixed+'#/pair-confirm');
 f=fixture(fixed+`#pair=ticket&key=secret&t=paired.trycloudflare.com&r=${rid}&install=1`);assert.equal(f.endpoint.pair,null);assert.deepEqual({...f.endpoint.credentials},saved);assert.equal(f.location.href,fixed);
 f=fixture(fixed+'#pair=ticket&key=secret&t=evil.test&r=bad');assert.throws(()=>f.endpoint.socketUrl(),/fresh QR/);
 for(const found of [{url:'https://evil.test',hash},{url:'https://ok.trycloudflare.com.evil.test',hash},{url:'https://ok.trycloudflare.com',hash:'../evil'},{url:'https://ok.trycloudflare.com',hash:'ABCDEF0123456789'},{url:'https://ok.trycloudflare.com',hash:1234567890123456}]){
  f=fixture(fixed,saved,found);assert.equal(await f.endpoint.locate(),false);assert.deepEqual(f.stored(),saved);assert.equal(f.endpoint.socketUrl(),`wss://saved.trycloudflare.com/ws?v=${hash}`);assert.deepEqual(f.redirects,[]);
 }
 // A pre-open failure locates and tries a changed endpoint immediately; unchanged endpoints back off.
 f=fixture(fixed);let t=f.context.KrillMobile.transport.create({endpoint:f.endpoint,credentials:f.endpoint.credentials});
 t.connect();await flush();f.sockets[0].emit('error');await flush();assert.equal(f.requests.length,1);assert.equal(f.requests[0].options.cache,'no-store');assert.equal(f.stored().url,'https://fresh.trycloudflare.com');assert.equal(f.timers.size,1);
 const retry=[...f.timers.values()][0];assert.equal(retry.ms,0);retry.fn();await flush();assert.equal(f.sockets[1].url,`wss://fresh.trycloudflare.com/ws?v=${hash}`);t.close();
 f=fixture(fixed,{...saved,rid:undefined});t=f.context.KrillMobile.transport.create({endpoint:f.endpoint,credentials:f.endpoint.credentials});t.connect();await flush();f.sockets[0].emit('error');await flush();assert.equal(f.requests.length,0);assert.equal([...f.timers.values()][0].ms,1000);t.close();
 f=fixture(fixed+'?session=7#/host/local',saved,{url:'https://fresh.trycloudflare.com',hash:'fedcba9876543210'});t=f.context.KrillMobile.transport.create({endpoint:f.endpoint,credentials:f.endpoint.credentials});t.connect();await flush();f.sockets[0].emit('error');await flush();assert.deepEqual(f.redirects,['/m/fedcba9876543210/?session=7#/host/local']);assert.equal(f.timers.size,0);t.close();
 f=fixture(fixed);t=f.context.KrillMobile.transport.create({endpoint:f.endpoint,credentials:f.endpoint.credentials});t.connect();await flush();f.sockets[0].emit('open');f.sockets[0].emit('close',1008);await flush();assert.equal(f.requests.length,0);assert.equal(f.timers.size,0);assert.equal(t.blocked,true);t.close();
 // Returning to the app (visibilitychange/pageshow/online → resume) reconnects at once instead of waiting out the backoff.
 f=fixture(fixed);t=f.context.KrillMobile.transport.create({endpoint:f.endpoint,credentials:f.endpoint.credentials});t.connect();await flush();f.sockets[0].emit('error');await flush();assert.equal(f.sockets.length,1);assert.equal(t.blocked,false);t.resume();await flush();assert.equal(f.sockets.length,2);t.close();
 // Authentication saves endpoint metadata, and terminal sockets share the main URL.
 f=fixture(fixed+`#pair=ticket&key=${Buffer.alloc(32).toString('base64url')}&t=paired.trycloudflare.com&r=${rid}`,null);
 let authenticated;t=f.context.KrillMobile.transport.create({endpoint:f.endpoint,pair:f.endpoint.pair,onAuthenticated:auth=>{authenticated=auth;f.context.localStorage.setItem('minikrill-mobile-v2',JSON.stringify(auth));}});
 t.connect();await flush();const socket=f.sockets[0];socket.emit('open');socket.emit('message',JSON.stringify({version:2,challenge:'bc'.repeat(32)}));await flush();
 const server=new C.Channel(new Uint8Array(32),C.concat(C.unhex('bc'.repeat(32)),new Uint8Array(32).fill(1)),false);
 socket.emit('message',server.seal(new TextEncoder().encode(JSON.stringify({device_id:'new-device',credential:'cd'.repeat(32)}))));socket.emit('message',server.seal(frame(1,Buffer.concat([v(1),v(1),v(0)]))));await flush();
 socket.emit('message',server.seal(frame(0x65,Buffer.concat([v(6),string(JSON.stringify({ok:true,data:{sessions:[]}}))]))));await flush();assert(t.ready);assert.equal(authenticated.rid,rid);assert.equal(authenticated.url,'https://paired.trycloudflare.com');assert.equal(authenticated.hash,hash);assert.equal(f.stored().credential,'cd'.repeat(32));const terminal=t.terminalConnect();assert.equal(terminal.socket.url,socket.url);assert.equal(terminal.credentials.rid,rid);
 // Back from a long background (or a new network): resume(stale) reconnects at once instead of probing a likely dead socket.
 {const n=f.sockets.length;t.resume(true);await flush();assert.equal(f.sockets.length,n+1);assert.equal(f.sockets.at(-1).url,socket.url);assert(socket.closed);}t.close();
 // A hung connection handshake gives up after 20 s, so locate/fallback and retry run instead of waiting out the TCP timeout.
 f=fixture(fixed);t=f.context.KrillMobile.transport.create({endpoint:f.endpoint,credentials:f.endpoint.credentials});t.connect();await flush();{const guard=[...f.timers.values()].find(x=>x.ms===20000);assert(guard);guard.fn();await flush();assert(f.sockets[0].closed);assert.equal(f.requests.length,1);assert([...f.timers.values()].some(x=>x.ms===0));}t.close();
 // In-app QR scan (endpoint.adopt): t/r or an https tunnel origin pairs in place; another app version or LAN origin opens the link; a LAN code in the fixed app says what to do.
 {const key=Buffer.alloc(32).toString('base64url'),other='b'.repeat(32);f=fixture(fixed);const next=f.endpoint.adopt(`https://elsewhere.example/m/${hash}/#pair=ab12&key=${key}&t=scan.trycloudflare.com&r=${other}`);assert.equal(f.endpoint.socketUrl(),`wss://saved.trycloudflare.com/ws?v=${hash}`);next.use();assert.equal(next.ticket,'ab12');assert.equal(next.secret,key);assert.equal(f.endpoint.socketUrl(),`wss://scan.trycloudflare.com/ws?v=${hash}`);
  f=fixture(fixed);f.endpoint.adopt(`https://quick.trycloudflare.com/#pair=ab12&key=${key}`).use();assert.equal(f.endpoint.socketUrl(),`wss://quick.trycloudflare.com/ws?v=${hash}`);
  f=fixture(fixed,null);f.endpoint.adopt(`${fixed}#pair=ab12&key=${key}&r=${other}`).use();assert.equal(f.endpoint.socketUrl(),`wss://krill-mobile.pages.dev/r/${other}?v=${hash}`);
  assert.equal(f.endpoint.adopt(`https://krill-mobile.pages.dev/m/fedcba9876543210/#pair=ab12&key=${key}&r=${rid}`),null);assert.throws(()=>f.endpoint.adopt(`http://192.168.0.5:52931/#pair=ab12&key=${key}`),/Choose Relay server.*New QR code/);
  for(const bad of ['hello','https://example.com/','https://example.com/#pair=zz&key=k',`javascript:x#pair=ab&key=${key}`])assert.throws(()=>f.endpoint.adopt(bad),/Not a Krill pairing code/);
  f=fixture('http://127.0.0.1:1234/');assert.equal(f.endpoint.adopt(`http://127.0.0.1:1234/#pair=ab12&key=${key}`).secret,key);assert.equal(f.endpoint.adopt(`http://10.0.0.2:1234/#pair=ab12&key=${key}`),null);}
 // Pure DO WebSocket relay mode (no t=) and automatic fallback when tunnel remains unreachable (e.g. 530).
 f=fixture(fixed+`#pair=ticket&key=${Buffer.alloc(32).toString('base64url')}&r=${rid}`,null);
 assert.equal(f.endpoint.socketUrl(),`wss://krill-mobile.pages.dev/r/${rid}?v=${hash}`);
 f=fixture(fixed,saved,{url:'https://saved.trycloudflare.com',hash});t=f.context.KrillMobile.transport.create({endpoint:f.endpoint,credentials:f.endpoint.credentials});
 t.connect();await flush();assert.equal(f.sockets[0].url,`wss://saved.trycloudflare.com/ws?v=${hash}`);
 f.sockets[0].emit('error');await flush();
 const relayRetry=[...f.timers.values()][0];relayRetry.fn();await flush();
 assert.equal(f.sockets[1].url,`wss://krill-mobile.pages.dev/r/${rid}?v=${hash}`);t.close();
 console.log('PASS fixed-origin fragment cleanup, saved destination protection, rid/url/hash persistence, DO WS relay & fallback, shared socket URLs, strict rendezvous validation, hash redirect and pre-open retry');
}
// Cross-language fixed screen fixtures: wide Hangul, combining ZWJ, colors and scroll ops.
const T=sandbox.MkTerminalCodec,model={};
T.update(Buffer.from('3000000021ac02f0a204010400501801030203020161000000000003ed959c0103e2808d01090201020302ff0080e80503030201','hex').subarray(5),model);
assert.equal(model.cols,80);assert.equal(model.lines,24);assert.equal(model.rows[3][0].ch,'a');assert.equal(model.rows[3][1].ch,'한');assert.equal(model.rows[3][1].zero,'\u200d');assert.equal(model.cursor.column,2);
const historyFixture=T.history(Buffer.from('100000002609e807030a02010178000000000000','hex').subarray(5));assert.equal(historyFixture.total,1000);assert.equal(historyFixture.pages,3);assert.equal(historyFixture.rows[0][0].ch,'x');
const before=model.rows[3];T.update(Buffer.concat([v(301),v(70000),v(1),v(1),v(1),v(2)]),model);assert.equal(model.rows[2],before);
for(let n=0;n<128;n++){const bytes=crypto.randomBytes(n);try{T.update(bytes,{cols:80,lines:24,rows:Array.from({length:24},()=>[])});}catch{}try{T.history(bytes);}catch{}}
assert.equal(sandbox.KrillMobile.terminal.keys.length,20);
const dir=fs.mkdtempSync('/tmp/m9pwa-'),repo=path.join(dir,'repo'),home=path.join(dir,'home'),profile=path.join(dir,'account');
fs.mkdirSync(repo);fs.mkdirSync(home);fs.mkdirSync(profile);fs.mkdirSync(path.join(dir,'paste'),{mode:0o700});
const env={...process.env,KRILL_STATE_DIR:dir,HOME:home,TMPDIR:path.join(dir,'paste')};
const log=fs.openSync(path.join(dir,'holder.log'),'a');let holder,daemon=0,phone,other,webhookServer,files=[];
const deadline=setTimeout(()=>{console.error('PWA E2E deadline exceeded');cleanup();process.exit(1);},150_000);
function cli(args){return execFileSync(mk,args,{env,encoding:'utf8',timeout:20000,stdio:['ignore','pipe','pipe']}).trim();}
function jsonCli(args){return JSON.parse(cli([...args,'--json']));}
function git(args){return execFileSync('git',args,{cwd:repo,env,encoding:'utf8'}).trim();}
function v(value){let n=BigInt(value),out=[];do{let b=Number(n&127n);n>>=7n;out.push(b|(n?128:0));}while(n);return Buffer.from(out);}
function string(s){const bytes=Buffer.from(s);return Buffer.concat([v(bytes.length),bytes]);}
function frame(k,body){const h=Buffer.alloc(5);h.writeUInt32LE(body.length+1);h[4]=k;return Buffer.concat([h,body]);}
function reader(bytes){let at=0;return{u(){let n=0n,shift=0n;for(;;){const b=bytes[at++];n|=BigInt(b&127)<<shift;if(!(b&128))return n;shift+=7n;}},text(){const n=Number(this.u()),s=bytes.subarray(at,at+n).toString();at+=n;return s;}};}
async function until(fn){const end=Date.now()+15000;while(Date.now()<end){try{const value=await fn();if(value)return value;}catch{}await new Promise(r=>setTimeout(r,30));}throw Error('condition timed out');}
function cleanup(){clearTimeout(deadline);webhookServer?.close();phone?.socket.close();other?.socket.close();if(holder){try{const list=cli(['ls']).split('\n');for(const line of list){const id=line.split('\t')[0];if(/^\d+$/.test(id))cli(['kill',id]);}}catch{}holder.kill('SIGKILL');}if(daemon)try{process.kill(daemon,'SIGKILL');}catch{}for(const f of files)try{fs.unlinkSync(f);}catch{}fs.closeSync(log);fs.rmSync(dir,{recursive:true,force:true,maxRetries:5,retryDelay:100});}
async function connect(url,credential){
 const u=new URL(url),socket=new WebSocket(`ws://${u.host}/ws`);socket.binaryType='arraybuffer';const messages=[],waiting=[];let failure;
 socket.addEventListener('message',e=>{const w=waiting.shift();if(w)w.resolve(e.data);else messages.push(e.data);});
 socket.addEventListener('close',()=>{failure=Error('WS closed');for(const w of waiting.splice(0))w.reject(failure);});
 const next=()=>messages.length?Promise.resolve(messages.shift()):failure?Promise.reject(failure):new Promise((resolve,reject)=>waiting.push({resolve,reject}));
 await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});
 const challenge=JSON.parse(await next()),nonce=crypto.randomBytes(32),secret=credential?C.sha256(C.unhex(credential.credential)):C.unbase64(new URLSearchParams(u.hash.slice(1)).get('key'));
 const channel=new C.Channel(secret,C.concat(C.unhex(challenge.challenge),nonce));
 socket.send(JSON.stringify({token:credential?null:new URLSearchParams(u.hash.slice(1)).get('pair'),device_id:credential?credential.device_id:null,client_nonce:nonce.toString('hex')}));
 socket.send(channel.seal(new TextEncoder().encode('{"label":"Non-secure HTTP test"}')));
 const authenticated=JSON.parse(new TextDecoder().decode(channel.open(await next()))),hello=Buffer.from(channel.open(await next())),r=reader(hello.subarray(5));r.u();r.u();const features=r.u();r.text();daemon=Number(r.u());
 socket.send(channel.seal(frame(2,Buffer.concat([v(1),v(1),v(features),v(0),string('pure-js-e2e')]))));
 let serial=Promise.resolve();
 return{socket,authenticated,async rpc(request){const operation=serial.then(async()=>{socket.send(channel.seal(frame(0x45,Buffer.concat([v(7),string(JSON.stringify(request))]))));const bytes=Buffer.from(channel.open(await next()));assert.equal(bytes[4],0x65);const r=reader(bytes.subarray(5));assert.equal(r.u(),6n);const response=JSON.parse(r.text());if(!response.ok)throw Error(response.error);return response.data;});serial=operation.catch(()=>{});return operation;}};
}
(async()=>{
 try {
  await fixedOriginTests();
  await require('./mobile_session.cjs').main();
  git(['init','-b','mobile-test']);git(['config','user.email','test@example.invalid']);git(['config','user.name','M9 test']);fs.writeFileSync(path.join(repo,'example.txt'),'before\n');git(['add','.']);git(['commit','-m','fixture']);fs.appendFileSync(path.join(repo,'example.txt'),'after\n');
  fs.writeFileSync(path.join(profile,'statusline_cache.json'),JSON.stringify({five_hour:{utilization:23},seven_day:{utilization:47}}));
  fs.writeFileSync(path.join(dir,'accounts.toml'),`[active]\nclaude = "claude-host"\n[[accounts]]\nid = "mobile-test-account"\nprovider = "claude"\nlabel = "Mobile fixture"\nprofile_dir = ${JSON.stringify(profile)}\n`);
  fs.writeFileSync(path.join(dir,'agents.toml'),'[agent.codex]\nname = "Mock Codex"\nlaunch = ["/bin/sh", "-c", "printf \'mock-agent-ready\\\\n\'; exec /bin/cat"]\n[agent.claude]\nname = "Mock Claude"\nlaunch = ["/bin/sh", "-c", "printf \'mock-agent-ready\\\\n\'; exec /bin/cat"]\n');
  fs.writeFileSync(path.join(dir,'waiting.json'),'{"hook_event_name":"PermissionRequest"}');
  holder=spawn(path.join(bin,'krill-holder'),[daemonBin,path.join(dir,'run/krilld.sock')],{env,stdio:['ignore',log,log]});await until(()=>fs.existsSync(path.join(dir,'run/krilld.sock')));
  daemon=await new Promise((resolve,reject)=>{const socket=net.createConnection(path.join(dir,'run/krilld.sock'));let bytes=Buffer.alloc(0);socket.setTimeout(5000,()=>socket.destroy(Error('hello timeout')));socket.on('error',reject);socket.on('data',part=>{bytes=Buffer.concat([bytes,part]);if(bytes.length>=4&&bytes.length>=4+bytes.readUInt32LE()){const r=reader(bytes.subarray(5));r.u();r.u();r.u();r.text();resolve(Number(r.u()));socket.destroy();}});});
  const shell=`stty -echo; sleep 0.15; '${mk}' hook claude <'${dir}/waiting.json' >/dev/null; printf 'READY\\n'; while IFS= read -r line; do printf 'got:%s\\n' "$line"; done`;
  const session=Number(cli(['new','--cwd',repo,'--','/bin/sh','-c',shell]));await until(()=>cli(['read',String(session)]).includes('READY'));
  const run=jsonCli(['orchestration','run-create','--title','Mobile run']);const runId=run.id||run.run_id||run.run?.id;assert(runId,JSON.stringify(run));
  const task=jsonCli(['orchestration','task-create','--run',runId,'--title','Mobile task']);
  const question=jsonCli(['orchestration','ask','--from','mobile-worker','--to','coordinator','--question','Proceed with fixture?']);
  const gate=jsonCli(['orchestration','gate-create','--from','test-worker','--question','Deploy fixture?','--options','Approve,Deny','--run',runId]);const gateId=gate.id||gate.gate_id;assert(gateId,JSON.stringify(gate));
  const offer=jsonCli(['mobile','pair','--address',keep?(process.env.M9_BROWSER_ADDRESS||'127.0.0.1:0'):'127.0.0.1:0']);const url=offer.PairingOffer.url,base=new URL(url).origin;
  for(const name of ['/','/app.js','/app.css','/i18n.js','/qr.js','/sw.js','/crypto.js','/client.js','/terminal.js','/screens/home.js','/screens/workspaces.js','/screens/chat.js','/screens/session.js','/screens/details.js','/screens/settings.js','/manifest.json','/icon.svg','/icon-192.png','/icon-512.png','/icon-maskable-512.png','/apple-touch-icon.png']){const response=await fetch(base+name);assert.equal(response.status,200);assert.equal(response.headers.get('content-encoding'),'gzip');assert.deepEqual(Buffer.from(await response.arrayBuffer()),fs.readFileSync(path.join(assets,name==='/'?'index.html':name.slice(1))));}
  if(keep){let terminalSession=null;const extraSessions=[];if(process.env.M9_UI_QA){
    const worktree=path.join(dir,'feature-worktree'),second=path.join(dir,'api'),outside=path.join(dir,'notes');
    git(['worktree','add','-b','feature/mobile',worktree]);fs.mkdirSync(second);fs.mkdirSync(outside);
    execFileSync('git',['init','-b','main'],{cwd:second,env});execFileSync('git',['-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','--allow-empty','-m','initial'],{cwd:second,env});
    for(const cwd of [repo,worktree,second,outside])extraSessions.push(Number(cli(['new','--cwd',cwd,'--','/bin/sh','-c',"printf 'SHELL READY\\n'; exec /bin/cat"])));
  }if(process.env.M9_LIVE_QA){
    const generator=path.join(dir,'terminal.py');
    fs.writeFileSync(generator,`import os, sys, tty, termios, fcntl, struct\nfor i in range(20000):\n print('%06d history 한글 ' % i + 'x'*110, flush=True)\nprint('\\x1b[31mLIVE RED\\x1b[0m 한글 e\\u0301', flush=True)\ntty.setraw(0)\nwhile True:\n data=os.read(0,4096)\n if not data: break\n rows,cols,_,_=struct.unpack('HHHH',fcntl.ioctl(0,termios.TIOCGWINSZ,b'\\0'*8))\n os.write(1,('\\r\\nKEYS:'+data.hex()+' SIZE:%dx%d\\r\\n'%(cols,rows)).encode())\n`);
    terminalSession=Number(cli(['new','--cwd',repo,'--','/usr/bin/python3','-u',generator]));
    for(let i=0;i<7;i++)cli(['new','--cwd',repo,'--','/bin/sh','-c',"printf 'IDLE\\n'; exec /bin/cat"]);
    await until(()=>cli(['read',String(terminalSession)]).includes('LIVE RED'));
  }clearTimeout(deadline);fs.writeFileSync(metadata,JSON.stringify({dir,repo,url,session,terminalSession,extraSessions,base,daemon,gateId,profile}));console.log('Browser fixture ready');await new Promise(r=>process.once('SIGTERM',r));return;}
  phone=await connect(url);let data=await phone.rpc({action:'snapshot'});const same=await phone.rpc({action:'snapshot',since:data.etag});assert.deepEqual(same,{unchanged:true,etag:data.etag});const card=data.sessions.find(s=>s.id===session);assert(card);assert.equal(card.state,'Waiting');assert.equal(card.branch,'mobile-test');assert.equal(card.added,1);assert(card.lines.some(l=>l.includes('READY')));assert(data.orchestration.gates.some(g=>g.id===gateId));assert(data.orchestration.tasks.length>=1);assert(data.orchestration.questions.some(q=>q.id===question.id));
  // Mobile workspace creation uses registered repository keys and the desktop git/setup path.
  assert.equal(data.capabilities.worktree_create,true);const repository=data.repositories.find(r=>r.worktree_keys.includes(card.worktree_key))?.key;assert(repository);
  for(const request of [{repository:'/tmp/forged',name:'bad'},{repository,name:' '},{repository,name:'bad',cwd:'/tmp'},{repository,name:'bad',setup_hash:'한'.repeat(64)},{repository,name:'bad',setup_hash:'aa'.repeat(32),skip_setup:true}])await assert.rejects(phone.rpc({action:'create_workspace',...request}));
  fs.writeFileSync(path.join(repo,'minikrill.toml'),'[worktree]\nsetup = "printf MOBILE_SETUP_OK"\n');
  const draft={action:'create_workspace',repository,name:'phone-workspace',base:'mobile-test'},review=await phone.rpc(draft);assert.equal(review.needs_setup,true);assert.equal(review.script,'printf MOBILE_SETUP_OK');assert.equal(review.hash.length,64);
  fs.writeFileSync(path.join(repo,'minikrill.toml'),'[worktree]\nsetup = "printf UPDATED_SETUP_OK"\n');
  const changed=await phone.rpc({...draft,setup_hash:review.hash});assert.equal(changed.needs_setup,true);assert.notEqual(changed.hash,review.hash);
  const created=await phone.rpc({...draft,setup_hash:changed.hash});assert.equal(created.branch,'phone-workspace');assert(created.session);assert(fs.existsSync(path.join(created.path,'.git')));await until(()=>cli(['read',String(created.setup_session)]).includes('UPDATED_SETUP_OK'));
  await until(async()=>{const snapshot=await phone.rpc({action:'snapshot'});return snapshot.sessions.some(s=>s.id===created.session&&s.worktree_key===created.workspace);});
  const skipped=await phone.rpc({...draft,name:'phone-skip-setup',skip_setup:true});assert.equal(skipped.branch,'phone-skip-setup');assert(skipped.session);assert(fs.existsSync(path.join(skipped.path,'.git')));
  await phone.rpc({action:'close_terminal',session:skipped.session});fs.unlinkSync(path.join(repo,'minikrill.toml'));
  console.log('PASS mobile workspaces: real worktree creation, registered key validation, changed setup hash review, approved setup, skip and terminal snapshot');
  await assert.rejects(phone.rpc({action:'approve',session,value:'y'}),/historical request.*Confirm in Term/);assert(!cli(['read',String(session)]).includes('got:y'));
  await assert.rejects(phone.rpc({action:'approve',session,value:'n'}),/historical request.*Confirm in Term/);assert(!cli(['read',String(session)]).includes('got:n'));
  await phone.rpc({action:'prompt',session,text:'안녕 모바일'});await until(()=>cli(['read',String(session)]).includes('got:안녕 모바일'));
  const photo=await phone.rpc({action:'photo',session,mime:'image/png',data:'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/a9sAAAAASUVORK5CYII='});files.push(photo.path);assert.equal(fs.statSync(photo.path).mode&0o777,0o600);assert.equal(fs.readFileSync(photo.path).subarray(0,8).toString('hex'),'89504e470d0a1a0a');await phone.rpc({action:'prompt',session,text:''});await until(()=>cli(['read',String(session)]).includes('mobile-'));
  const accounts=await phone.rpc({action:'accounts'});assert(accounts.accounts.some(a=>a.id==='mobile-test-account'));const switched=await phone.rpc({action:'switch_account',provider:'claude',account_id:'mobile-test-account'});assert(switched.accounts.some(a=>a.id==='mobile-test-account'&&a.active));assert(fs.readFileSync(path.join(dir,'active-accounts.env'),'utf8').includes(profile));
  await phone.rpc({action:'gate_reply',id:gateId,decision:'Approve'});assert.equal(JSON.parse(fs.readFileSync(path.join(dir,'orchestration.json'))).gates.find(g=>g.id===gateId).status,'resolved');
  await phone.rpc({action:'question_reply',id:question.id,body:'Proceed from mobile'});assert(JSON.parse(fs.readFileSync(path.join(dir,'orchestration.json'))).messages.some(m=>m.reply_to===question.id&&m.body==='Proceed from mobile'));
  const diff=await phone.rpc({action:'git',session,path:'example.txt'});assert(diff.patch.includes('+after'));const outside=await phone.rpc({action:'git',session,path:'../../secrets'});assert.equal(outside.diff_status,'outside_scope');assert.equal(outside.patch,'');
  const handoff=await phone.rpc({action:'handoff',session,agent:'codex'});assert(handoff.to_session);await until(()=>cli(['read',String(handoff.to_session)]).includes('MK HANDOFF'));
  const saved={device_id:phone.authenticated.device_id,credential:phone.authenticated.credential};phone.socket.close();await new Promise(r=>setTimeout(r,80));
  const rejectedCode=await new Promise((resolve,reject)=>{const u=new URL(url),ws=new WebSocket(`ws://${u.host}/ws`);ws.binaryType='arraybuffer';ws.addEventListener('error',reject,{once:true});ws.addEventListener('message',e=>{const ch=JSON.parse(e.data),nonce=crypto.randomBytes(32),chan=new C.Channel(new Uint8Array(32),C.concat(C.unhex(ch.challenge),nonce));ws.send(JSON.stringify({token:null,device_id:saved.device_id,client_nonce:nonce.toString('hex')}));ws.send(chan.seal(new TextEncoder().encode('{"label":"Bad credential"}')));},{once:true});ws.addEventListener('close',e=>resolve(e.code),{once:true});});
  assert.equal(rejectedCode,1008);
  other=await connect(url,saved);const resumed=await other.rpc({action:'snapshot'});assert(resumed.sessions.some(s=>s.id===session));
  // Daemon-owned notifications reach all providers with no desktop process running.
  const posts=[];webhookServer=require('node:http').createServer((req,res)=>{let body='';req.on('data',b=>body+=b);req.on('end',()=>{posts.push({path:req.url,body});res.writeHead(204);res.end();});});await new Promise(r=>webhookServer.listen(0,'127.0.0.1',r));
  const webhookBase=`http://127.0.0.1:${webhookServer.address().port}`;
  fs.writeFileSync(path.join(dir,'settings.toml'),`notifications = false\n[[webhooks]]\nprovider = "ntfy"\nurl = "${webhookBase}/ntfy"\n[[webhooks]]\nprovider = "telegram"\nurl = "${webhookBase}/telegram"\nchat_id = "fixture"\n[[webhooks]]\nprovider = "discord"\nurl = "${webhookBase}/discord"\n`);
  for(const [file,event] of [['working','UserPromptSubmit'],['waiting','PermissionRequest'],['done','Stop']])fs.writeFileSync(path.join(dir,file+'.json'),JSON.stringify({hook_event_name:event}));
  const hookCommand=`while [ ! -f '${dir}/webhook-go' ]; do sleep 0.02; done; '${mk}' hook claude < '${dir}/working.json'; '${mk}' hook claude < '${dir}/waiting.json'; '${mk}' hook claude < '${dir}/done.json'; printf 'SECRET_TERMINAL_CONTENT\\n'; exec /bin/cat`;
  const notifiedSession=Number(cli(['new','--cwd',repo,'--','/bin/sh','-c',hookCommand]));fs.writeFileSync(path.join(dir,'webhook-go'),'');await until(()=>posts.length===6);
  for(const provider of ['ntfy','telegram','discord']){const pair=posts.filter(p=>p.path==='/'+provider);assert.equal(pair.length,2);for(const p of pair){assert(p.body.includes(base+'/?session='+notifiedSession));assert(!p.body.includes('SECRET'));assert(!p.body.includes('pair='));assert(!p.body.includes('key='));}assert(pair[0].body.includes('waiting for input'));assert(pair[1].body.includes('completed'));}
  console.log('Daemon webhook E2E passed: ntfy, Telegram, Discord, waiting/done, credential-free deep links and no app process');
  const {fragment}=await other.rpc({action:'install_handoff'});assert.match(fragment,/^pair=[0-9a-f]{64}&key=[\w-]{43}$/);await require('./mobile_install.cjs').main({base,fragment});
  console.log('PWA E2E passed: non-secure pure JS E2EE, gzip assets, approval, IME text, photo 0600, account switch, gate reply, diff, mock handoff and credential reconnect');
 } finally {cleanup();}
})().catch(error=>{console.error(error.stack);process.exitCode=1;});
