// Independent review: execute from Krill repository root; no source mutations or daemon.
'use strict';
const fs=require('fs'),path=require('path'),assert=require('assert/strict');
const dir=path.resolve('crates/krilld/tests');
function loadChat(){let code=fs.readFileSync(path.join(dir,'mobile_chat.cjs'),'utf8');code=code.slice(0,code.indexOf('async function main()'));return new Function('require','__dirname',code+';return {fixture,deferred,flush};')(require,dir);}
function loadTransport(){let code=fs.readFileSync(path.join(dir,'mobile_transport.cjs'),'utf8');code=code.slice(0,code.indexOf('for(const [stage,label]'));return new Function('require','__dirname',code+';return {harness,flush};')(require,dir);}
async function remount(){
 const {fixture,deferred,flush}=loadChat();
 for(const edit of [null,'different','edit-back','failure']){
  const f=fixture();await flush();const p=deferred();f.handler=async q=>q.action==='prompt'?p.promise:{generation:'g1',items:[],next_offset:q.offset,bound:true};
  f.input('send once');f.submit();await flush();f.remount();await flush();f.submit();await flush();assert.equal(f.calls.filter(c=>c.action==='prompt').length,1,'remount keeps submit lock');
  if(edit==='different')f.input('new draft');if(edit==='edit-back'){f.input('new draft');f.input('send once');}
  if(edit==='failure')p.reject(Error('not delivered'));else p.resolve({sent:true});await flush();
  const expected=edit==='different'?'new draft':edit?'send once':'';
  assert.equal(f.find('chat-input').value,expected);assert.equal(f.state.drafts.get(7),expected||undefined);
  if(edit===null){f.submit();await flush();assert.equal(f.calls.filter(c=>c.action==='prompt').length,1,'completed unedited prompt cannot be resubmitted');}
  console.log('remount',edit||'unchanged','=>',JSON.stringify(expected));f.dispose();
 }
}
function unitServer(f){let total=0;const page=q=>{const start=q.offset,end=Math.min(total,start+128,q.end_offset??Infinity);return {generation:'g1',items:Array.from({length:end-start},(_,i)=>({id:start+':'+i,kind:'assistant',text:'Message '+(start+i)})),next_offset:end,bound:true};};f.handler=async q=>page(q);return {page,set total(n){total=n;}};}
async function history(){
 const {fixture,flush}=loadChat(),f=fixture();await flush();const server=unitServer(f);
 for(let n=1;n<=300;n++){server.total=n;f.update();await flush();}
 for(let start=49;start>=0;start--){f.button('Load earlier messages').action();await flush();const values=f.entry.items.map(x=>Number(x.text.slice(8)));assert.deepEqual(Array.from(values),Array.from({length:250},(_,i)=>start+i));assert.equal(f.calls.at(-1).end_offset,start+1);}
 assert.equal(f.entry.items.length,250);assert.equal(f.errors.length,0);f.button('Back to latest').action();await flush();assert.equal(f.entry.items[0].text,'Message 50');assert.equal(f.entry.items.at(-1).text,'Message 299');f.dispose();console.log('history: 300 incremental unit pages, 50 exact earlier reads, cap/latest preserved');
}
async function byteHistory(){
 const {fixture,flush}=loadChat(),f=fixture();await flush();const records=Array.from({length:360},(_,i)=>({kind:'assistant',text:i===48||i===49?'Identical text':`다른 길이 Message ${i}`})),offsets=[0],expected=[];
 for(const r of records)offsets.push(offsets.at(-1)+Buffer.byteLength(JSON.stringify(r)+'\n'));let total=0;
 f.handler=async q=>{const start=offsets.indexOf(q.offset),bound=q.end_offset===undefined?Infinity:offsets.indexOf(q.end_offset);assert(start>=0&&bound>=start);const end=Math.min(total,start+128,bound);return {generation:'g1',items:records.slice(start,end).map((r,i)=>({...r,id:q.offset+':'+i})),next_offset:offsets[end],bound:true};};
 const batches=[7,1,13,4,2];let batch=0;
 while(total<records.length){const start=total;total=Math.min(records.length,total+batches[batch++%batches.length]);for(let i=start;i<total;i++)expected.push({id:offsets[start]+':'+(i-start),text:records[i].text});f.update();await flush();}
 assert.deepEqual(Array.from(f.entry.items,x=>x.id),expected.slice(-250).map(x=>x.id));let turns=0;
 while(!f.button('Load earlier messages').hidden){f.button('Load earlier messages').action();await flush();assert(++turns<100);const ids=Array.from(f.entry.items,x=>x.id),begin=expected.findIndex(x=>x.id===ids[0]);assert.deepEqual(ids,expected.slice(begin,begin+250).map(x=>x.id));assert.equal(new Set(ids).size,ids.length);assert.equal(f.entry.items.length,250);}
 assert.deepEqual(Array.from(f.entry.items,x=>x.id),expected.slice(0,250).map(x=>x.id));const twins=f.entry.items.filter(x=>x.text==='Identical text');assert.equal(twins.length,2);assert.notEqual(twins[0].id,twins[1].id);assert.equal(f.errors.length,0);f.dispose();console.log('byteHistory: mixed short byte pages, clipped page, duplicate texts and chronology preserved');
}
async function historyRace(){
 const {fixture,deferred,flush}=loadChat(),f=fixture();await flush();const server=unitServer(f);
 for(let n=1;n<=300;n++){server.total=n;f.update();await flush();}
 const earlier=deferred(),live=deferred();f.handler=q=>q.end_offset!==undefined?earlier.promise:live.promise;
 f.button('Load earlier messages').action();await flush();f.update();await flush();earlier.resolve(server.page({offset:49,end_offset:50}));await flush();
 const before=Array.from(f.entry.items,x=>x.id);assert.equal(f.entry.items[0].text,'Message 49');server.total=310;live.resolve(server.page({offset:300}));await flush();
 const after=Array.from(f.entry.items,x=>x.id);console.log('historyRace first/last:',f.entry.items[0].text,f.entry.items.at(-1).text);f.dispose();assert.deepEqual(after,before,'a live poll queued during earlier-page load must not mutate the historical view');
}
async function locate(){
 const {harness,flush}=loadTransport();
 for(const located of [false,true])for(const phase of ['connecting','ready']){
  const h=harness();let finishLocate,fallbacks=0;const lookup=new Promise(r=>finishLocate=r),events=[];
  const t=h.context.KrillMobile.transport.create({credentials:{device_id:'aa',credential:'bb'},endpoint:{socketUrl:()=> 'wss://desktop.test/ws',locate:()=>lookup,fallback(){fallbacks++;}},onStatus:s=>events.push(s)});
  const first=t.connect();await flush();h.sockets.at(-1).emit('error');await flush();t.reconnect();await flush();const current=h.sockets.at(-1);
  if(phase==='ready'){await h.advance(current);h.response(current,{sessions:[]});await flush();assert(t.ready);}
  const count=events.length;finishLocate(located);await flush();await first;assert(!current.closed);assert.equal(t.state.phase,phase);assert.equal(fallbacks,0);assert.equal(events.length,count);
  t.reconnect();await flush();assert.notEqual(h.sockets.at(-1),current);t.close();console.log('locate',located,phase,'preserved');
 }
}
async function discoverySideEffects(){
 const {harness,flush}=loadTransport();
 for(const delayed of ['fetch','json'])for(const operation of ['reconnect','setPair','close']){
  const saved={device_id:'a'.repeat(64),credential:'b'.repeat(64),rid:'c'.repeat(32),url:'https://saved.trycloudflare.com',hash:'0123456789abcdef'},h=harness(saved,undefined,true);
  let fetchDone,jsonDone,fetchSignal;const fetchPromise=new Promise(r=>fetchDone=r),jsonPromise=new Promise(r=>jsonDone=r),reply={ok:true,json:()=>jsonPromise};h.context.fetch=(_url,options)=>{fetchSignal=options.signal;return fetchPromise;};
  const {pending,socket}=await h.start();socket.emit('error');await flush();if(delayed==='json'){fetchDone(reply);await flush();}
  if(operation==='setPair')h.transport.setPair({ticket:'new',secret:'secret'});else h.transport[operation]();await flush();
  const current=operation==='close'?null:h.sockets.at(-1),route=h.endpoint.socketUrl(),stored=h.store.get('minikrill-mobile-v2'),endpointCredentials=JSON.stringify(h.endpoint.authenticated({})),events=h.events.length;
  jsonDone({hash:'fedcba9876543210',url:'https://stale.trycloudflare.com'});fetchDone(reply);await pending;await flush();
  assert.equal(h.endpoint.socketUrl(),route);assert.equal(h.store.get('minikrill-mobile-v2'),stored);assert.equal(JSON.stringify(h.endpoint.authenticated({})),endpointCredentials);assert.equal(h.redirects.length,0);assert.equal(h.events.length,events);assert(fetchSignal.aborted);if(current)assert(!current.closed);h.transport.close();console.log('discovery',delayed,operation,'side effects suppressed');
 }
}
(async()=>{let failed=0;for(const [name,test] of Object.entries({remount,history,byteHistory,historyRace,locate,discoverySideEffects})){try{await test();console.log('PASS',name);}catch(e){failed++;console.error('FAIL',name,e.stack);}}process.exitCode=failed?1:0;})();
