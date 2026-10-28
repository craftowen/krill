// Chat failure-path regressions. No daemon, user session, network or browser required.
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
class Element extends EventTarget{
 constructor(tag,text,cls){super();this.tag=tag;this.textContent=text;this.className=cls;this.children=[];this.dataset={};this.attrs={};this.style={};this.scrollTop=0;this.clientHeight=100;this.value='';}
 append(...nodes){for(const n of nodes){n.remove();n.parentElement=this;this.children.push(n);}}
 insertBefore(node,next){node.remove();const i=this.children.indexOf(next);node.parentElement=this;if(i<0)this.children.push(node);else this.children.splice(i,0,node);}
 replaceChildren(...nodes){for(const n of this.children)n.parentElement=null;this.children=[];this.append(...nodes);}
 remove(){if(this.parentElement){this.parentElement.children=this.parentElement.children.filter(n=>n!==this);this.parentElement=null;}}
 setAttribute(k,v){this.attrs[k]=v;}
 descendants(){return this.children.flatMap(n=>[n,...n.descendants()]);}
 get scrollHeight(){return this.children.length*100;}
 get offsetTop(){return this.parentElement?this.parentElement.children.indexOf(this)*100:0;}
 click(){this.action?.();}
}
const deferred=()=>{let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return {promise,resolve,reject};};
const flush=async()=>{for(let i=0;i<30;i++)await Promise.resolve();};
function fixture(options={}){
 const buttons=[],calls=[],sheets=[],errors=[],tasks=[],timers=new Map();let id=0,term=0;
 const copied=[];
 const doc=Object.assign(new EventTarget(),{hidden:false,visibilityState:'visible',createElementNS:(_,tag)=>new Element(tag),body:new Element('body'),execCommand:()=>true});
 const sandbox={Event,document:doc,isSecureContext:true,navigator:{clipboard:{writeText:async text=>{copied.push(text);}}},Date:options.Date||Date,setTimeout:(fn)=>{timers.set(++id,fn);return id;},clearTimeout:id=>timers.delete(id),KrillMobile:{},ResizeObserver:options.ResizeObserver};
 vm.createContext(sandbox);vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/chat.js'),'utf8'),sandbox);
 const state={sessions:new Map([[7,{id:7,state:'Waiting',cwd:'/work'}]]),connection:{phase:options.phase||'ready'},drafts:new Map(),snapshot:{capabilities:options.capabilities||{}}};
 const root=new Element('root'),el=(...args)=>new Element(...args);
 let handler=options.handler|| (async request=>request.action==='chat_poll'?{generation:'g1',agent:'claude',bound:true,items:[],next_offset:request.offset}:{sent:true});
 let prepare=async()=>({mime:'image/jpeg',data:'encoded'});
 const ctx={el,state,button(label,action,cls){const b=el('button',label,cls);b.action=action;buttons.push(b);return b;},rpc:async request=>{calls.push(request);return handler(request);},photo:(...args)=>prepare(...args),act(fn){const p=Promise.resolve().then(fn).catch(e=>errors.push(e));tasks.push(p);return p;},refresh:async()=>{},sheet(title,_,__,render){const body=el('body');render(body);sheets.push({title,body});}};
 let view=sandbox.KrillMobile.chat.mount(root,{ctx,sessionId:7,visible:options.visible,onQuickActions:options.onQuickActions,onSwitchTerm:()=>term++});
 const find=cls=>root.descendants().find(n=>n.className===cls),button=label=>root.descendants().find(n=>n.tag==='button'&&n.textContent===label);
 return {insertText:text=>view.insertText(text),root,state,ctx,calls,buttons,sheets,errors,tasks,find,button,timers,doc,copied,M:sandbox.KrillMobile,get term(){return term;},get entry(){return state.chatCache.get(7);},set handler(h){handler=h;},set prepare(h){prepare=h;},update(){view.update();},setVisible(v){view.setVisible(v);},remount(){view.unmount();view=sandbox.KrillMobile.chat.mount(root,{ctx,sessionId:7,onSwitchTerm:()=>term++});},dispose(){view.unmount();},input(text){const input=find('chat-input');input.value=text;input.selectionStart=input.selectionEnd=text.length;input.dispatchEvent(new Event('input'));},submit(){find('chat-composer').dispatchEvent(new Event('submit',{cancelable:true}));},async poll(data){handler=async req=>req.action==='chat_poll'?data:{sent:true};view.update();await flush();}};
}
function unitServer(f){let total=0;const page=q=>{const start=q.offset,end=Math.min(total,start+128,q.end_offset??Infinity);return {generation:'g1',items:Array.from({length:end-start},(_,i)=>({id:start+':'+i,kind:'assistant',text:'Message '+(start+i)})),next_offset:end,bound:true};};f.handler=async q=>page(q);return {page,set total(n){total=n;}};}
async function historyRace(){
 const f=fixture();await flush();const server=unitServer(f);
 for(let n=1;n<=1050;n++){server.total=n;f.update();await flush();}
 const earlier=deferred(),live=deferred();f.handler=q=>q.end_offset!==undefined?earlier.promise:live.promise;
 f.button('Load earlier messages').action();await flush();f.update();await flush();earlier.resolve(server.page({offset:49,end_offset:50}));await flush();
 const before=Array.from(f.entry.items,x=>x.id);assert.equal(f.entry.items[0].text,'Message 49');server.total=1060;live.resolve(server.page({offset:1050}));await flush();
 const after=Array.from(f.entry.items,x=>x.id);console.log('historyRace first/last:',f.entry.items[0].text,f.entry.items.at(-1).text);f.dispose();assert.deepEqual(after,before,'a live poll queued during earlier-page load must not mutate the historical view');
}
function scroll(feed,top){feed.scrollTop=top;feed.dispatchEvent(new Event('scroll'));}
const historyPage=(start,end)=>({generation:'g1',agent:'claude',bound:true,page_start:start,next_offset:end,items:Array.from({length:end-start},(_,i)=>({id:`message${start+i}`,kind:'assistant',text:`Message ${start+i}`}))});
const historyHandler=async req=>req.end_offset!==undefined?historyPage(Math.max(0,req.end_offset-10),req.end_offset):req.latest?historyPage(200,230):historyPage(req.offset,req.offset);
async function automaticHistory(){
 const f=fixture({handler:historyHandler});await flush();const feed=f.find('chat-feed');
 assert.equal(f.calls.length,1,'initial tail does not eagerly backfill');feed.dispatchEvent(new Event('scroll'));assert.equal(f.calls.length,1,'bottom pin event does not backfill');
 Object.defineProperty(feed,'scrollHeight',{configurable:true,value:300});scroll(feed,100);assert.equal(f.calls.length,1,'layout-induced scroll clamp does not backfill');delete feed.scrollHeight;
 scroll(feed,500);const pending=deferred();f.handler=req=>req.end_offset!==undefined?pending.promise:historyHandler(req);
 scroll(feed,119);scroll(feed,80);scroll(feed,30);scroll(feed,0);await flush();
 assert.equal(f.calls.length,2,'burst of upward scroll events issues a single automatic request');assert.equal(f.calls.at(-1).end_offset,200);assert.equal(f.calls.at(-1).latest,true);assert.equal(f.button('Load earlier messages').disabled,true);
 // The reader keeps moving while the page is in flight: preserve the partial card now visible.
 scroll(feed,450);const anchor=feed.children[4],screenTop=anchor.offsetTop-feed.scrollTop;
 pending.resolve(historyPage(190,200));await flush();assert.equal(anchor.offsetTop-feed.scrollTop,screenTop,'response preserves current partial-card anchor, not the request-time scroll position');assert.equal(feed.scrollTop,1450);assert.equal(feed.children[14],anchor);assert(f.entry.live);assert.equal(f.entry.live.items[0].id,'message200');
 feed.dispatchEvent(new Event('scroll'));await flush();assert.equal(f.calls.length,2,'prepend-generated scroll event does not request another page');f.update();for(const [id,fn] of [...f.timers]){f.timers.delete(id);await fn();}assert.equal(f.calls.length,2,'reading history pauses live polling');
 f.handler=historyHandler;scroll(feed,100);await flush();assert.equal(f.calls.length,3,'a new upward scroll reads the next page');assert.equal(f.calls.at(-1).end_offset,190);assert.equal(f.entry.items[0].id,'message180');assert.equal(new Set(f.entry.items.map(x=>x.id)).size,f.entry.items.length);
 f.button('Back to latest').action();await flush();assert.equal(f.entry.items[0].id,'message200');assert.equal(feed.scrollTop,feed.scrollHeight);f.dispose();
 const live=deferred(),queued=fixture({handler:historyHandler});await flush();const qfeed=queued.find('chat-feed');queued.handler=req=>req.end_offset!==undefined?historyHandler(req):live.promise;queued.update();await flush();scroll(qfeed,110);scroll(qfeed,20);assert.equal(queued.calls.length,2,'upward intent waits for an active live poll');
 live.resolve(historyPage(230,231));await flush();assert.equal(queued.calls.length,3,'live poll finally drains the queued upward intent');assert.equal(queued.calls.at(-1).end_offset,200);assert.equal(queued.entry.items[0].id,'message190');queued.dispose();
 const cancel=fixture({handler:historyHandler});await flush();const cfeed=cancel.find('chat-feed'),late=deferred();cancel.handler=req=>req.end_offset!==undefined?late.promise:historyHandler(req);scroll(cfeed,100);await flush();assert.equal(cancel.button('Back to latest').hidden,false,'latest is available during the first earlier request');cancel.button('Back to latest').action();late.resolve(historyPage(190,200));await flush();assert.equal(cancel.entry.live,undefined);assert.equal(cancel.entry.items[0].id,'message200');assert.equal(cfeed.scrollTop,cfeed.scrollHeight);assert.equal(cancel.calls.filter(c=>c.end_offset!==undefined).length,1);assert.equal(cancel.calls.length,3,'latest polling resumes after the cancelled page settles');cancel.dispose();
 const nested=fixture({handler:historyHandler});await flush();scroll(nested.find('chat-feed'),100);await flush();const older=deferred();nested.handler=req=>req.end_offset!==undefined?older.promise:historyHandler(req);scroll(nested.find('chat-feed'),90);await flush();nested.button('Back to latest').action();older.resolve(historyPage(180,190));await flush();assert.equal(nested.entry.live,null);assert.equal(nested.entry.items[0].id,'message200','late second historical page cannot undo latest restore');nested.dispose();
 const capped=fixture({handler:async()=>historyPage(200,1200)});await flush();const capFeed=capped.find('chat-feed'),capPending=deferred();capped.handler=()=>capPending.promise;scroll(capFeed,100);await flush();scroll(capFeed,99850);const capAnchor=capFeed.children[998],capTop=capAnchor.offsetTop-capFeed.scrollTop;capPending.resolve(historyPage(190,200));await flush();assert.equal(capped.entry.items.length,1000);assert.equal(capFeed.children[998],capAnchor);assert.equal(capAnchor.offsetTop-capFeed.scrollTop,capTop,'a delayed prepend cannot evict the reader moved to the clipped tail');assert.equal(capped.entry.items[0].id,'message200','prepend is deferred when the current reader would fall outside the cap');capped.dispose();
 console.log('PASS automatic history: real scroll intent, no mount/layout/prepend backfill, burst single-flight, response-time anchor, live-poll queued intent, paused history and latest cancellation');
}
async function automaticHistoryFailures(){
 const f=fixture({handler:historyHandler});await flush();const feed=f.find('chat-feed');let attempts=0;f.handler=async req=>{if(req.end_offset!==undefined){attempts++;throw Error('earlier unavailable');}return historyHandler(req);};
 scroll(feed,110);await flush();assert.equal(attempts,1);assert.equal(f.entry.live,undefined);assert.equal(f.button('Load earlier messages').disabled,false);scroll(feed,70);scroll(feed,20);scroll(feed,0);f.update();await flush();assert.equal(attempts,1,'same-boundary failure cannot storm as upward events continue');
 scroll(feed,400);scroll(feed,100);await flush();assert.equal(attempts,2,'moving away and back explicitly rearms automatic retry');f.handler=historyHandler;f.button('Retry').action();await flush();assert.equal(f.entry.items[0].id,'message190','error Retry retries the historical page');f.dispose();
 const manual=fixture({handler:historyHandler});await flush();const mfeed=manual.find('chat-feed');manual.handler=async()=>{throw Error('busy');};scroll(mfeed,100);await flush();manual.handler=historyHandler;manual.button('Load earlier messages').action();await flush();assert.equal(manual.entry.items[0].id,'message190','manual fallback can retry a blocked automatic boundary');manual.dispose();
 // Reject the whole page without changing items/pages/live, including missing generation.
 for(const bad of [{next_offset:199},{generation:'g2'},{generation:undefined},{reset:true},{page_start:200},{page_start:-1}]){
  const invalid=fixture({handler:historyHandler});await flush();const before=invalid.entry.items.slice(),pages=JSON.stringify(invalid.entry.pages);invalid.handler=async()=>({...historyPage(190,200),...bad});scroll(invalid.find('chat-feed'),100);await flush();assert.deepEqual(invalid.entry.items,before);assert.equal(JSON.stringify(invalid.entry.pages),pages);assert.equal(invalid.entry.live,undefined);assert.equal(invalid.button('Load earlier messages').disabled,false);invalid.dispose();
 }
 const duplicate=fixture({handler:historyHandler});await flush();duplicate.handler=async()=>({...historyPage(190,200),items:[historyPage(190,200).items[0],historyPage(190,200).items[0],historyPage(200,201).items[0]]});scroll(duplicate.find('chat-feed'),100);await flush();assert.equal(new Set(duplicate.entry.items.map(x=>x.id)).size,duplicate.entry.items.length,'duplicates inside an incoming page are dropped as well');duplicate.dispose();
 console.log('PASS automatic history failures: no retry storm, move-away/manual/error retry, exact end/generation/start/reset validation and duplicate ID defense');
}
async function inactiveHistory(){
 const modes={terminal:f=>f.setVisible(false),document:f=>{f.doc.hidden=true;f.doc.visibilityState='hidden';f.doc.dispatchEvent(new Event('visibilitychange'));},cached:f=>{f.state.cached=true;f.update();},offline:f=>{f.state.connection.phase='disconnected';f.update();},host:f=>{f.state.chatCache=new Map();f.state.drafts=new Map();},disposed:f=>f.dispose(),aborted:f=>f.ctx.signal={aborted:true}};
 for(const [name,deactivate] of Object.entries(modes)){
  const f=fixture({handler:historyHandler});await flush();const feed=f.find('chat-feed'),entry=f.entry;deactivate(f);scroll(feed,100);f.button('Load earlier messages').action();await flush();assert.equal(f.calls.length,1,`${name} does not request older pages`);assert.equal(entry.items[0].id,'message200');f.dispose();
  const stale=fixture({handler:historyHandler});await flush();const pending=deferred(),sfeed=stale.find('chat-feed'),old=stale.entry;stale.handler=()=>pending.promise;scroll(sfeed,100);await flush();deactivate(stale);pending.resolve(historyPage(190,200));await flush();assert.equal(old.items[0].id,'message200',`${name} drops late older results`);assert.equal(old.live,undefined);assert.equal(stale.calls.length,2,`${name} does not issue a follow-up`);stale.dispose();
  const live=fixture({handler:historyHandler});await flush();const waiting=deferred(),previous=live.entry;live.handler=()=>waiting.promise;live.update();await flush();deactivate(live);waiting.resolve(historyPage(230,231));await flush();assert.equal(previous.items.length,30,`${name} drops a late live delta as well`);assert.equal(previous.offset,230);live.dispose();
 }
 // Even a hidden-then-visible transition before completion fences the earlier request.
 const revived=fixture({handler:historyHandler});await flush();const pending=deferred();revived.handler=req=>req.end_offset!==undefined?pending.promise:historyHandler(req);scroll(revived.find('chat-feed'),100);await flush();revived.setVisible(false);revived.setVisible(true);pending.resolve(historyPage(190,200));await flush();assert.equal(revived.entry.items[0].id,'message200');assert.equal(revived.entry.live,undefined);revived.dispose();
 // Queued scroll intent is discarded when the pane is hidden before the live poll settles.
 const queued=fixture({handler:historyHandler});await flush();const live=deferred();queued.handler=()=>live.promise;queued.update();await flush();scroll(queued.find('chat-feed'),100);queued.setVisible(false);live.resolve(historyPage(230,230));await flush();assert.equal(queued.calls.length,2);assert.equal(queued.entry.live,undefined);queued.dispose();
 console.log('PASS inactive history: terminal/document hidden, cached, offline, changed host, disposed/aborted do not request/apply earlier pages; hidden cycle and queued intent fenced');
}
async function renderBudgets(){
 const f=fixture({visible:false});await flush();const initial=f.calls.length;for(let n=0;n<10;n++){f.update();for(const [id,fn] of [...f.timers]){f.timers.delete(id);await fn();}await flush();}assert.equal(f.calls.length,initial,'hidden repeated update/timer polls are zero');assert.equal(f.timers.size,0);f.setVisible(true);await flush();assert.equal(f.calls.length,initial+1,'visibility restore catches up once');assert.equal(f.timers.size,1);
 f.doc.hidden=true;f.doc.visibilityState='hidden';f.doc.dispatchEvent(new Event('visibilitychange'));assert.equal(f.timers.size,0);f.update();await flush();assert.equal(f.calls.length,initial+1);f.doc.hidden=false;f.doc.visibilityState='visible';f.doc.dispatchEvent(new Event('visibilitychange'));await flush();assert.equal(f.calls.length,initial+2,'document foreground catches up once');
 f.input('');assert(!f.state.drafts.has(7),'empty drafts do not pin session caches');f.dispose();
 const incremental=fixture();await flush();await incremental.poll({bound:true,generation:'g1',next_offset:500,items:Array.from({length:500},(_,i)=>({id:'i'+i,kind:'assistant',text:'Message '+i}))});const liveFeed=incremental.find('chat-feed');let reads=0;const original=liveFeed.children;Object.defineProperty(liveFeed,'children',{configurable:true,get(){reads++;return original;},set(value){original.splice(0,original.length,...value);}});await incremental.poll({bound:true,generation:'g1',next_offset:500,items:[]});assert.equal(reads,0,'live no-op poll does not traverse/reconcile existing feed children');await incremental.poll({bound:true,generation:'g1',next_offset:501,items:[{id:'append',kind:'assistant',text:'Append'}]});assert(reads<10,'append reconciliation only touches the appended node');incremental.dispose();
 const cache=fixture();await flush();cache.dispose();cache.state.chatCache.clear();for(let n=0;n<20;n++)cache.state.chatCache.set(n,{items:[],retainedBytes:100});cache.state.drafts.set(0,'unsent');cache.state.chatCache.get(1).photo={status:'ready'};cache.state.chatCache.get(2).sending=true;cache.state.chatCache.get(3).mounts=1;cache.state.chatCache.get(4).live={items:[],offset:1};cache.M.chat.pruneCache(cache.state);assert.equal(cache.state.chatCache.size,8);for(const n of [0,1,2,3,4])assert(cache.state.chatCache.has(n));
 cache.state.chatCache.set(99,{retainedBytes:9*1024*1024});cache.M.chat.pruneCache(cache.state);assert(!cache.state.chatCache.has(99),'byte pressure removes inactive clean entries');for(const n of [0,1,2,3,4])assert(cache.state.chatCache.has(n));
 const pages=fixture();await flush();const server=unitServer(pages);for(let n=1;n<=2400;n++){server.total=n;pages.update();await flush();}assert(pages.entry.pages.length<=2048);assert.equal(pages.entry.items.length,1000);assert.equal(pages.entry.items[0].text,'Message 1400');pages.button('Load earlier messages').action();await flush();assert.equal(pages.entry.items[0].text,'Message 1399','pruned page metadata retains exact preceding byte boundary');
 const feed=pages.find('chat-feed'),nodes=feed.children.slice();let touched=0;const insert=feed.insertBefore.bind(feed);feed.insertBefore=(...args)=>{touched++;return insert(...args);};pages.update();await flush();assert.equal(touched,0,'paused/no-op update never reconciles feed children');assert.deepEqual(feed.children,nodes);pages.dispose();
 const bounded=fixture();await flush();await bounded.poll({bound:true,generation:'g1',next_offset:100,items:Array.from({length:100},(_,i)=>({id:'b'+i,kind:'assistant',text:'x'.repeat(15000)}))});assert(bounded.entry.retainedBytes<=2*1024*1024);assert(bounded.entry.items.length<100);assert.equal(bounded.entry.items[0]._clipped,true,'byte-trim retains replay information for its partial page');bounded.dispose();
 const lazy=fixture();await flush();await lazy.poll({bound:true,generation:'g1',next_offset:1,items:[{id:'huge',kind:'reasoning',text:'reasoning line\n'.repeat(5000)}]});const details=lazy.find('chat-reasoning');assert.equal(details.children.length,1,'large closed reasoning only creates its summary');details.open=true;details.dispatchEvent(new Event('toggle'));assert.equal(details.children.length,2);details.open=false;details.dispatchEvent(new Event('toggle'));assert.equal(lazy.entry.disclosures.size,0);lazy.dispose();
 console.log('PASS render budgets: hidden poll 0, one catch-up, cache 8 with draft/photo/sending/active pins, pages <=2048, exact older boundary, lazy disclosure');
}
async function tailFollow(){
 const observers=[];class RO{constructor(fn){this.fn=fn;this.targets=new Set();observers.push(this);}observe(n){this.targets.add(n);}unobserve(n){this.targets.delete(n);}disconnect(){this.targets.clear();}}
 const page=(from,to)=>({generation:'g1',agent:'claude',bound:true,page_start:0,next_offset:to,items:Array.from({length:to-from},(_,i)=>({id:'t'+(from+i),kind:'assistant',text:'Message '+(from+i)}))});
 const f=fixture({ResizeObserver:RO});await flush();await f.poll(page(0,30));f.dispose();
 // A cached transcript paints at the tail synchronously, before the reopen poll answers.
 const reopen=deferred();f.handler=()=>reopen.promise;f.remount();const feed=f.find('chat-feed'),ro=observers.at(-1);assert.equal(feed.children.length,30);assert.equal(feed.scrollTop,feed.scrollHeight,'first chat render is already at the latest message');
 assert(ro.targets.has(feed)&&ro.targets.has(feed.children.at(-1)),'feed and rendered cards are observed for late layout');
 Object.defineProperty(feed,'scrollHeight',{value:5000,configurable:true});ro.fn([]);assert.equal(feed.scrollTop,5000,'late code/image/disclosure growth keeps a following reader at the bottom');delete feed.scrollHeight;
 reopen.resolve(page(0,30));await flush();
 feed.scrollTop=feed.scrollHeight-feed.clientHeight;feed.dispatchEvent(new Event('scroll'));assert.equal(feed.scrollTop,2900,'scroll near the tail keeps following');
 // Scrolling up by 15px within the bottom 60px band must disengage following immediately so ResizeObserver / new messages do not snap back to bottom.
 feed.scrollTop=feed.scrollHeight-feed.clientHeight-15;feed.dispatchEvent(new Event('scroll'));Object.defineProperty(feed,'scrollHeight',{value:5500,configurable:true});ro.fn([]);assert.equal(feed.scrollTop,2885,'upward scroll within bottom 60px disengages following');delete feed.scrollHeight;
 feed.scrollTop=400;feed.dispatchEvent(new Event('scroll'));Object.defineProperty(feed,'scrollHeight',{value:6000,configurable:true});ro.fn([]);assert.equal(feed.scrollTop,400,'late growth never drags a reader who scrolled up');delete feed.scrollHeight;
 await f.poll(page(30,32));assert.equal(feed.children.length,32);assert.equal(feed.scrollTop,400,'new messages do not pull a reader who scrolled up');
 feed.scrollTop=feed.scrollHeight-feed.clientHeight;feed.dispatchEvent(new Event('scroll'));await f.poll(page(32,33));assert.equal(feed.scrollTop,feed.scrollHeight,'returning to the bottom resumes following');
 f.dispose();assert.equal(ro.targets.size,0,'unmount disconnects the observer');
 console.log('PASS chat tail: cached first render at bottom, ResizeObserver re-pin while following, scrolled-up reader kept in place, bottom resumes');
}
// 연결 전에 숨은 채 마운트돼도 ready 뒤 첫 페이지를 받아, Chat을 누른 프레임에 내용이 있다. 구버전 데몬이 latest를 거부하면 처음부터 읽는다.
async function hiddenPrefetchAndOldDaemon(){
 const page={generation:'g1',agent:'claude',bound:true,page_start:0,items:[{id:'a',kind:'assistant',text:'hello'}],next_offset:10};
 const f=fixture({visible:false,phase:'connecting',handler:async()=>page});
 await flush();assert.equal(f.calls.length,0);
 f.state.connection.phase='ready';f.update();await flush();assert.equal(f.calls.length,1);assert.equal(f.calls[0].latest,true);
 f.update();await flush();assert.equal(f.calls.length,1,'hidden Chat stops polling after the first page');
 f.setVisible(true);assert(f.root.descendants().some(n=>n.tag==='article'),'tapping Chat shows the prefetched page in the same frame');f.dispose();
 const old=fixture({visible:true,handler:async r=>{if(r.latest)throw Error('invalid mobile authentication or frame');return {...page,items:[{id:'b',kind:'assistant',text:'old'}],next_offset:5};}});old.state.snapshot={capabilities:{hybrid_chat:true}};old.remount();
 await flush();assert.equal(old.calls.length,3);assert.equal(old.calls[2].latest,undefined);assert.equal(old.calls[2].offset,0);assert(old.root.descendants().some(n=>n.tag==='article'),'old daemon still shows the transcript');old.dispose();
 const current=fixture({visible:true,handler:async()=>{throw Error('invalid mobile authentication or frame');}});current.state.snapshot={capabilities:{chat_tail:true}};current.remount();
 await flush();assert(current.calls.length>0&&current.calls.every(c=>c.latest),'a chat_tail daemon never drops the tail request');current.dispose();
 let fails=0;const flaky=fixture({visible:false,handler:async()=>{fails++;throw Error('busy');}});await flush();for(let i=0;i<5;i++){flaky.update();await flush();}assert.equal(fails,2,'hidden Chat stops retrying after two failures');flaky.dispose();
 console.log('PASS hidden Chat prefetches once after connecting; chat_tail-less daemon rejection falls back to offset polling');
}
async function orcaComposerAndMeta(){
 const f=fixture();await flush();await f.poll({generation:'g1',agent:'claude',bound:true,items:[{id:'0:0',kind:'user',text:'hi',at:'2026-10-24T03:48:10.000Z'},{id:'0:1',kind:'assistant',text:'done'}],next_offset:10});
 const metas=f.root.descendants().filter(n=>n.className==='chat-meta');assert.equal(metas.length,2,'user and agent messages carry a meta row');
 assert.equal(metas[0].children[0].className,'icon-button chat-copy');assert.equal(metas[0].children[1].className,'chat-time');assert.equal(metas[0].children[1].attrs.datetime,'2026-10-24T03:48:10.000Z');
 assert.equal(metas[1].children.length,1,'a message without a record time shows only the copy button');
 assert.equal(f.find('chat-input').placeholder,'Message {0}','a known agent gets the per-agent placeholder (this fixture has no translator)');
 assert.equal(f.buttons.find(b=>b.className==='composer-pill').hidden,true,'old daemons do not get a fictional model action');f.dispose();
 console.log('PASS Orca composer: copy/time under messages and capability-gated model action');
}
async function markdownFormatting(){
 const f=fixture();await flush();
 const text='**실패 지점은 확인했지만, 복구는 아직 못 했습니다.**\n\n- `pwd`뿐 아니라 **셸을 쓰지 않는 자바스크립트 한 줄**도 실패합니다.\n1. 첫 번째 단계\n2. **두 번째** 단계\n> 인용 첫 줄\n> 인용 `코드` 둘째 줄\n| 항목 | 상태 |\n| --- | :---: |\n| `build` | **통과** |\n### **확인 결과**\n[공식 문서](https://example.com/docs)와 *강조*, snake_case, \\*그대로\\*\n```js\nconst raw = "**그대로** <img src=x onerror=alert(1)>";\n```\n<img src=x onerror=alert(1)> [차단](javascript:alert)';
 await f.poll({generation:'g1',agent:'codex',bound:true,next_offset:1,items:[{id:'markdown',kind:'assistant',text}]});
 const nodes=f.find('chat-md').descendants(),byTag=tag=>nodes.filter(n=>n.tag===tag);
 assert.deepEqual(byTag('strong').map(n=>n.children.map(c=>c.textContent).join('')),['실패 지점은 확인했지만, 복구는 아직 못 했습니다.','셸을 쓰지 않는 자바스크립트 한 줄','두 번째','통과','확인 결과']);
 assert.equal(byTag('code')[0].textContent,'pwd');
 assert.equal(byTag('ol').length,1);assert.equal(byTag('ol')[0].className,'chat-ol');assert.equal(byTag('ul').length,1);assert.equal(byTag('li').length,3);assert.equal(byTag('ol')[0].children.filter(n=>n.tag==='li').length,2);
 assert.equal(byTag('blockquote').length,1);assert.equal(byTag('blockquote')[0].className,'chat-quote');assert(byTag('blockquote')[0].descendants().some(n=>n.tag==='code'&&n.textContent==='코드'));
 assert.equal(byTag('table').length,1);assert.equal(byTag('table')[0].className,'chat-table');assert.equal(byTag('th').length,2);assert.equal(byTag('td').length,2);
 assert.equal(byTag('em').length,1);
 assert.equal(byTag('a').length,1,'only safe links become anchors');
 assert.equal(byTag('a')[0].attrs.href,'https://example.com/docs');
 assert.equal(byTag('pre')[0].children[0].textContent,'const raw = "**그대로** <img src=x onerror=alert(1)>";','code blocks retain literal Markdown and HTML');
 assert.equal(byTag('img').length,0,'message HTML is displayed as text');
 assert(nodes.some(n=>n.textContent?.includes('snake_case')));
 assert.equal(byTag('strong').some(n=>n.textContent?.includes('그대로')),false);
 const chatInput=f.find('chat-input');Object.defineProperty(chatInput,'scrollHeight',{configurable:true,get:()=>200});
 f.input('long multiline');assert.equal(chatInput.style.height,'140px','sizeInput clamps to 140px max');
 Object.defineProperty(chatInput,'scrollHeight',{configurable:true,get:()=>18});
 f.input('short');assert.equal(chatInput.style.height,'28px','sizeInput clamps to 28px min');
 f.dispose();console.log('PASS mobile Markdown: Korean bold, inline code, ol/blockquote/table, headings, emphasis, safe links, literal code/HTML and sizeInput');
}
async function quickActions(){
 let opened=0;const f=fixture({onQuickActions:()=>opened++});await flush();const quick=f.find('btn chat-quick-actions-btn');assert(quick);assert.equal(quick.attrs['aria-label'],'Quick actions');assert.equal(quick.title,'Quick actions');quick.action();assert.equal(opened,1);
 assert.equal(f.insertText('\r\n'),false);const input=f.find('chat-input');assert.equal(f.insertText('/krill-orchestration '),true);assert.equal(input.value,'/krill-orchestration ');assert.equal(input.selectionEnd,input.value.length);
 input.value='use OLD please';input.selectionStart=4;input.selectionEnd=7;f.insertText('/ship-dev');assert.equal(input.value,'use /ship-dev please');assert.equal(f.state.drafts.get(7),input.value);assert.equal(input.selectionStart,13);assert(!f.calls.some(q=>q.action==='prompt'),'insert never submits');
 input.selectionStart=input.selectionEnd=undefined;f.insertText('\r\nnext\r\nline\r\n');assert(input.value.endsWith('next\nline'));f.remount();assert.equal(f.find('chat-input').value,'use /ship-dev pleasenext\nline');
 f.dispose();
}
// Same-generation tail refresh is a merge, including open disclosures and reader anchors.
async function stableLatestRefresh(){
 let now=10000;const Clock=class extends Date{static now(){return now;}},items=Array.from({length:30},(_,i)=>({id:'stable'+i,kind:i===0?'reasoning':'assistant',text:'Message '+i})),page=items=>({generation:'g1',agent:'claude',bound:true,items,page_start:0,next_offset:items.length});
 const f=fixture({Date:Clock,handler:async()=>page(items)});await flush();const feed=f.find('chat-feed'),card=feed.children[0],details=f.find('chat-reasoning');details.open=true;details.dispatchEvent(new Event('toggle'));feed.scrollTop=400;feed.dispatchEvent(new Event('scroll'));const reader=feed.children[4];
 f.setVisible(false);now+=5000;f.handler=async()=>page([...items,{id:'next',kind:'assistant',text:'New reply'}]);f.setVisible(true);await flush();assert.equal(f.calls.at(-1).latest,true);assert.equal(feed.children[0],card);assert.equal(f.find('chat-reasoning'),details);assert(details.open);assert.equal(f.entry.disclosures.size,1);assert.equal(feed.scrollTop,400);assert.equal(feed.children[4],reader);
 // A shifted window retains overlapping cards, and a changed item updates its content in place.
 f.setVisible(false);now+=5000;f.handler=async()=>page([...items.slice(10).map(item=>item.id==='stable12'?{...item,text:'Updated reply'}:item),{id:'last',kind:'assistant',text:'Latest'}]);const updated=feed.children[12];f.setVisible(true);await flush();assert.equal(feed.children[0],card);assert.equal(feed.children[12],updated);assert(updated.descendants().some(n=>n.textContent==='Updated reply'));assert.equal(feed.scrollTop,400);
 // Trim from above the reader while keeping the surviving card's offset.
 const many=Array.from({length:1000},(_,i)=>({id:'many'+i,kind:'assistant',text:'Many '+i}));await f.poll({generation:'g2',bound:true,reset:true,items:many,next_offset:1000});feed.scrollTop=400;feed.dispatchEvent(new Event('scroll'));const kept=feed.children[4];await f.poll({generation:'g2',bound:true,items:[{id:'many1000',kind:'assistant',text:'New'}],next_offset:1001});assert.equal(feed.children[3],kept);assert.equal(feed.scrollTop,300);
 // Model chrome above the feed to prove error show/hide compensates the reader's viewport top.
 const status=feed.parentElement.children[1];feed.getBoundingClientRect=()=>({top:status.hidden?0:50});for(const child of feed.children)child.getBoundingClientRect=()=>({top:(status.hidden?0:50)+child.offsetTop-feed.scrollTop,bottom:(status.hidden?0:50)+child.offsetTop+100-feed.scrollTop});
 const readerTop=kept.getBoundingClientRect().top;f.handler=async()=>{throw Error('offline');};f.update();await flush();assert.equal(kept.getBoundingClientRect().top,readerTop);await f.poll({generation:'g2',bound:true,items:[],next_offset:1001});assert.equal(kept.getBoundingClientRect().top,readerTop);f.dispose();
 console.log('PASS CLS chat: latest merge retains cards/disclosure/following, updates content, trims and error chrome preserve reader anchor');
}
function key(f,value,options={}){const event=new Event('keydown',{cancelable:true});Object.assign(event,{key:value,...options});f.find('chat-input').dispatchEvent(event);return event;}
async function composerDiscovery(){
 const capabilities={command_catalog:true,mention_files:true,composer_models:true};
 const f=fixture({capabilities});await flush();
 let catalogs=0;const mention=deferred();
 f.handler=async req=>{
  if(req.action==='command_catalog'){catalogs++;return {supported:true,items:[{name:'review',text:'$review',kind:'Skill',description:'Review code'},{name:'help',text:'/help',kind:'Command',description:'CLI help'},{name:'unsafe',text:'/bad\nexecute',kind:'Command'}]};}
  if(req.action==='mention_files')return mention.promise;
  if(req.action==='composer_models')return {supported:true,current:'a',items:[{id:'a',label:'Model A'},{id:'b',label:'Model B'}]};
  if(req.action==='set_model')return {applied:true};
  return {bound:true,agent:'codex',items:[],next_offset:0};
 };
 f.input('/');await flush();assert.equal(f.find('chat-composer-options').children.length,2,'catalog includes installed skills and commands, drops control text');
 assert.equal(f.find('chat-input').attrs['aria-expanded'],'true');
 key(f,'ArrowDown');assert.equal(f.find('chat-composer-options').children[1].attrs['aria-selected'],'true');
 const selected=key(f,'Enter');assert(selected.defaultPrevented);assert.equal(f.find('chat-input').value,'/help ');assert.equal(f.state.drafts.get(7),'/help ');assert(!f.calls.some(c=>c.action==='prompt'),'selection never sends');
 f.input('/review');await flush();assert.equal(catalogs,1,'same provider/workspace catalog is cached');key(f,'Enter');assert.equal(f.find('chat-input').value,'$review ','provider invocation text is preserved');
 f.input('/');await flush();f.find('chat-input').dispatchEvent(new Event('compositionstart'));assert.equal(f.find('chat-composer-suggestions').hidden,true);assert.equal(key(f,'Enter',{isComposing:true}).defaultPrevented,false);f.find('chat-input').dispatchEvent(new Event('compositionend'));await flush();key(f,'Escape');assert.equal(f.find('chat-composer-suggestions').hidden,true);
 f.input('Inspect @src');for(const [id,fn] of [...f.timers]){if(id===1)continue;f.timers.delete(id);fn();}await flush();
 mention.resolve({items:[{path:'src/a "quoted" file.js'},{path:'../escape'},{path:'/absolute'},{path:'line\nbreak'}],truncated:true});await flush();
 assert.equal(f.find('chat-composer-options').children.length,1,'only safe relative paths are inserted');key(f,'Enter');assert.equal(f.find('chat-input').value,'Inspect '+JSON.stringify('src/a \"quoted\" file.js')+' ');assert(!f.calls.some(c=>c.action==='prompt'));
 f.buttons.find(b=>b.className==='composer-pill').action();await flush();assert.equal(f.find('chat-composer-options').children.length,2);key(f,'ArrowDown');key(f,'Enter');await flush();assert.equal(f.term,0);assert.equal(f.buttons.find(b=>b.className==='composer-pill').textContent,'Model B');assert(f.calls.some(c=>c.action==='set_model'&&c.model==='b'));assert(!f.calls.some(c=>c.action==='prompt'),'model selection uses only explicit RPC');
 f.dispose();
 const unsupported=fixture({capabilities,handler:async req=>req.action==='composer_models'?{supported:false,items:[{id:'launch-only',label:'Launch only'}],reason:'No live control'}:{bound:true,agent:'codex',items:[],next_offset:0}});await flush();unsupported.buttons.find(b=>b.className==='composer-pill').action();await flush();assert.equal(unsupported.find('chat-composer-options').children.length,0);assert(unsupported.find('chat-composer-suggestions').descendants().some(n=>n.textContent==='No live control'));assert.equal(unsupported.term,0);unsupported.dispose();
 const changing=deferred(),model=fixture({capabilities,handler:async req=>req.action==='composer_models'?{supported:true,items:[{id:'real',label:'Real model'}]}:req.action==='set_model'?changing.promise:{bound:true,agent:'codex',items:[],next_offset:0}});await flush();model.input('unsent draft');model.buttons.find(b=>b.className==='composer-pill').action();await flush();key(model,'Enter');await flush();assert.equal(model.buttons.find(b=>b.className==='composer-pill').textContent,'Model','pending request is not a model change');assert.equal(model.find('primary composer-send').disabled,true);assert.equal(model.find('chat-input').value,'unsent draft');changing.resolve({applied:false,reason:'Native composer busy'});await flush();assert.equal(model.buttons.find(b=>b.className==='composer-pill').textContent,'Model');assert(model.find('chat-composer-suggestions').descendants().some(n=>n.textContent==='Native composer busy'));assert.equal(model.find('primary composer-send').disabled,false);assert.equal(model.term,0);model.dispose();
 const commandModel=fixture({capabilities,handler:async req=>req.action==='command_catalog'?{items:[{name:'model',text:'/model',kind:'Command'}]}:req.action==='composer_models'?{supported:true,items:[{id:'real',label:'Real model'}]}:{bound:true,agent:'codex',items:[],next_offset:0}});await flush();commandModel.input('/model');await flush();key(commandModel,'Enter');await flush();assert.equal(commandModel.find('chat-input').value,'');assert.equal(commandModel.find('chat-composer-options').children[0].children[0].textContent,'Real model');assert(!commandModel.calls.some(c=>c.action==='prompt'),'native /model selection routes inline');assert.equal(commandModel.term,0);key(commandModel,'Escape');commandModel.input('/model keep this draft');commandModel.find('chat-input').selectionStart=commandModel.find('chat-input').selectionEnd=6;commandModel.find('chat-input').dispatchEvent(new Event('input'));await flush();key(commandModel,'Enter');await flush();assert.equal(commandModel.find('chat-input').value,' keep this draft','inline model routing preserves text outside the unchanged trigger');key(commandModel,'Escape');assert.equal(commandModel.find('chat-input').value,' keep this draft');assert(!commandModel.calls.some(c=>c.action==='prompt'));commandModel.dispose();
 const late=deferred(),race=fixture({capabilities});await flush();race.handler=req=>req.action==='command_catalog'?late.promise:Promise.resolve({bound:true,agent:'codex',items:[],next_offset:0});race.input('/');await flush();race.input('new draft');late.resolve({items:[{text:'$stale',name:'stale'}]});await flush();assert.equal(race.find('chat-composer-suggestions').hidden,true);assert.equal(race.find('chat-input').value,'new draft');race.dispose();
 const detached=deferred(),host=fixture({capabilities});await flush();host.handler=req=>req.action==='command_catalog'?detached.promise:Promise.resolve({});host.input('/');await flush();host.state.chatCache=new Map();detached.resolve({items:[{text:'/stale',name:'stale'}]});await flush();assert.equal(host.find('chat-composer-options').children.length,0,'late old-host results do not populate choices');host.dispose();
 console.log('PASS composer discovery: provider text, skills+commands, safe path text, keyboard/IME, no auto-send, cache/stale-host guards, truthful models without Term');
}
async function composerRaces(){
 const capabilities={command_catalog:true,mention_files:true,composer_models:true},modelButton=f=>f.buttons.find(b=>b.className==='composer-pill'&&f.root.descendants().includes(b));
 for(const field of ['account_id','profile_id','provider','foreground','groupKey','command_context']){
  const f=fixture({capabilities});await flush();const pending=deferred(),sess=f.state.sessions.get(7);sess[field]='A';f.update();await flush();f.handler=req=>req.action==='command_catalog'?pending.promise:historyHandler(req);f.input('/private');await flush();assert.equal(f.calls.at(-1).action,'command_catalog');sess[field]='B';f.update();pending.resolve({items:[{text:'/private-A',name:'private-A'}]});await flush();assert.equal(f.find('chat-composer-suggestions').hidden,true,`${field} change hides pending old catalog`);assert.equal(f.find('chat-composer-options').children.length,0);key(f,'Enter');assert.equal(f.find('chat-input').value,'/private');
  f.handler=async req=>req.action==='command_catalog'?{items:[{text:'/private-B',name:'private-B'}]}:historyHandler(req);f.input('/private');await flush();const old=f.find('chat-composer-options').children[0];assert.equal(old.children[0].textContent,'/private-B');sess[field]='C';old.action();assert.equal(f.find('chat-input').value,'/private','a stale rendered button cannot insert across a scope change even before update');f.dispose();
 }
 const raw=fixture({capabilities,handler:async req=>({...await historyHandler(req),agent:'codex'})});await flush();raw.state.sessions.get(7).agent='codex';raw.update();await flush();const rawCatalog=deferred();raw.handler=req=>req.action==='command_catalog'?rawCatalog.promise:Promise.resolve({generation:'g1',agent:'codex',bound:true,items:[],next_offset:req.offset});raw.input('/');await flush();raw.state.sessions.get(7).agent='claude';raw.update();await flush();assert.equal(raw.entry.agent,'codex','transcript agent can lag behind the raw snapshot agent');rawCatalog.resolve({items:[{text:'/codex-only',name:'codex-only'}]});await flush();assert.equal(raw.find('chat-composer-suggestions').hidden,true);assert.equal(raw.find('chat-composer-options').children.length,0,'raw agent change fences a catalog even while entry.agent masks chatAgent');assert.equal(raw.find('chat-input').value,'/');raw.dispose();
 // A -> B -> A changes must not revive the original request or its cached catalog.
 const cycle=fixture({capabilities});await flush();const catalog=deferred(),sess=cycle.state.sessions.get(7);sess.account_id='A';cycle.update();cycle.handler=req=>req.action==='command_catalog'?catalog.promise:historyHandler(req);cycle.input('/');await flush();sess.account_id='B';cycle.update();sess.account_id='A';cycle.update();catalog.resolve({items:[{text:'/old-A',name:'old-A'}]});await flush();assert.equal(cycle.find('chat-composer-suggestions').hidden,true);cycle.handler=async req=>req.action==='command_catalog'?{items:[{text:'/fresh-A',name:'fresh-A'}]}:historyHandler(req);cycle.input('/');await flush();assert.equal(cycle.find('chat-composer-options').children[0].children[0].textContent,'/fresh-A');assert.equal(cycle.calls.filter(q=>q.action==='command_catalog').length,2);cycle.dispose();
 const known={supported:true,current:'a',items:[{id:'a',label:'Model A'},{id:'b',label:'Model B'}]};
 for(const data of [{supported:false,current:null,items:[]},{...known,current:null},{...known,current:'unlisted'},null]){
  const f=fixture({capabilities});await flush();f.handler=req=>req.action==='composer_models'?known:historyHandler(req);modelButton(f).action();await flush();assert.equal(modelButton(f).textContent,'Model A');f.button('Close suggestions').action();f.handler=async req=>{if(req.action!=='composer_models')return historyHandler(req);if(data===null)throw Error('model unavailable');return data;};modelButton(f).action();assert.equal(modelButton(f).textContent,'Model','fresh model authority is uncertain until the reply');await flush();assert.equal(modelButton(f).textContent,'Model');assert.equal(f.entry.model,null);assert.equal(f.calls.filter(q=>q.action==='composer_models').length,2,'every reopen queries current live support/current');f.dispose();
 }
 const staleRead=fixture({capabilities});await flush();const modelRead=deferred();staleRead.handler=req=>req.action==='composer_models'?modelRead.promise:historyHandler(req);modelButton(staleRead).action();await flush();staleRead.state.sessions.get(7).profile_id='new-profile';staleRead.update();modelRead.resolve(known);await flush();assert.equal(staleRead.find('chat-composer-suggestions').hidden,true);assert.equal(modelButton(staleRead).textContent,'Model');staleRead.dispose();
 for(const applied of [true,false]){
  const f=fixture({capabilities});await flush();const pending=deferred();f.handler=req=>req.action==='composer_models'?known:req.action==='set_model'?pending.promise:historyHandler(req);f.input('draft before model');modelButton(f).action();await flush();key(f,'ArrowDown');key(f,'Enter');await flush();const token=f.entry.modelPending;assert(token);assert.equal(f.find('primary composer-send').disabled,true);f.remount();await flush();f.input('newer unsent draft');assert.equal(f.entry.modelPending,token);assert.equal(f.find('primary composer-send').disabled,true,'pending model operation locks remounted Send');assert.equal(modelButton(f).disabled,true);f.submit();modelButton(f).action();await flush();assert(!f.calls.some(q=>q.action==='prompt'));assert.equal(f.calls.filter(q=>q.action==='set_model').length,1);assert.equal(f.calls.filter(q=>q.action==='composer_models').length,1,'remount cannot start another model picker while pending');pending.resolve({applied,model:'b'});await flush();assert.equal(f.entry.modelPending,null);assert.equal(f.find('primary composer-send').disabled,false);assert.equal(modelButton(f).textContent,applied?'Model B':'Model A');assert.equal(f.find('chat-input').value,'newer unsent draft');assert.equal(f.state.drafts.get(7),'newer unsent draft');f.dispose();
 }
 const changed=fixture({capabilities});await flush();const applying=deferred();changed.handler=req=>req.action==='composer_models'?known:req.action==='set_model'?applying.promise:historyHandler(req);modelButton(changed).action();await flush();key(changed,'ArrowDown');key(changed,'Enter');await flush();const cs=changed.state.sessions.get(7);cs.account_id='B';changed.update();cs.account_id=undefined;changed.update();applying.resolve({applied:true,model:'b'});await flush();assert.equal(changed.entry.modelPending,null);assert.equal(changed.entry.model,null,'old scope success cannot claim a model after an A -> B -> A scope transition');assert.equal(modelButton(changed).textContent,'Model');changed.dispose();
 const host=fixture({capabilities});await flush();const oldSet=deferred(),newSet=deferred();let count=0;host.handler=req=>req.action==='composer_models'?known:req.action==='set_model'?(++count===1?oldSet.promise:newSet.promise):historyHandler(req);modelButton(host).action();await flush();key(host,'ArrowDown');key(host,'Enter');await flush();const oldEntry=host.entry;host.state.chatCache=new Map();host.state.drafts=new Map([[7,'new host draft']]);host.remount();await flush();modelButton(host).action();await flush();key(host,'ArrowDown');key(host,'Enter');await flush();const newToken=host.entry.modelPending;oldSet.resolve({applied:true,model:'old-host-model'});await flush();assert.equal(host.entry.modelPending,newToken,'old host completion cannot release the new host lock');assert.equal(host.entry.model.id,'a');assert.equal(host.find('primary composer-send').disabled,true);assert.equal(host.state.drafts.get(7),'new host draft');assert.equal(oldEntry.modelPending,null);newSet.resolve({applied:true,model:'b'});await flush();assert.equal(modelButton(host).textContent,'Model B');host.dispose();
 const pinned=fixture({capabilities});await flush();const wait=deferred();pinned.handler=req=>req.action==='composer_models'?known:req.action==='set_model'?wait.promise:historyHandler(req);modelButton(pinned).action();await flush();key(pinned,'ArrowDown');key(pinned,'Enter');await flush();const entry=pinned.entry;pinned.dispose();for(let n=100;n<120;n++)pinned.state.chatCache.set(n,{items:[],retainedBytes:100});pinned.M.chat.pruneCache(pinned.state);assert.equal(pinned.state.chatCache.get(7),entry,'a pending model operation pins its shared entry even without draft/mount');wait.resolve({applied:false});await flush();assert.equal(entry.modelPending,null);
 console.log('PASS composer races: account/profile/provider epochs fence catalogs/models; uncertain current clears, live model reads stay fresh; shared model operation locks remount, preserves edits, fences old host and pins pending cache');
}
async function composerProgress(){
 const f=fixture();await flush();const sent=deferred();f.handler=async req=>req.action==='prompt'?sent.promise:{generation:'g1',bound:true,agent:'claude',items:[],next_offset:0};
 f.input('Review this');f.submit();assert.equal(f.find('chat-live-text').textContent,'Sending…');assert.equal(f.find('chat-send-receipt').hidden,false);assert(f.find('chat-send-receipt').textContent.includes('Review this'));await flush();sent.resolve({sent:true});await flush();assert.equal(f.find('chat-live-text').textContent,'Accepted. Awaiting agent response…');assert.equal(f.find('chat-input').value,'');
 const sess=f.state.sessions.get(7);sess.state='Working';sess.state_since_unix_ns='2';f.update();await flush();assert.equal(f.find('chat-live-text').textContent,'Working');
 await f.poll({generation:'g1',bound:true,agent:'claude',items:[{id:'u',kind:'user',text:'Review this'},{id:'t',kind:'tool_call',tool_name:'read_file',file_path:'src/main.rs'}],next_offset:3});assert.equal(f.find('chat-send-receipt').hidden,true,'actual transcript confirmation removes provisional acknowledgement');assert(f.find('chat-live-text').textContent.includes('read_file'));
 sess.state='Waiting';sess.state_since_unix_ns='3';f.update();await flush();assert.equal(f.find('chat-live-text').textContent,'Waiting for input');sess.state='Done';sess.state_since_unix_ns='4';f.update();await flush();assert.equal(f.find('chat-live-text').textContent,'Completed');
 f.state.connection.phase='disconnected';f.update();assert.equal(f.find('chat-live-text').textContent,'Offline. Reconnect to update activity.');f.dispose();
 const failure=fixture();await flush();failure.handler=async req=>{if(req.action==='prompt')throw Error('timeout');return {items:[],next_offset:0};};failure.input('keep draft');failure.submit();await flush();assert.equal(failure.state.drafts.get(7),'keep draft');assert.equal(failure.find('chat-input').value,'keep draft');assert.equal(failure.find('chat-send-receipt').hidden,false);assert.equal(failure.find('chat-live-text').textContent,'Send not confirmed. Draft kept; check activity before retrying.');assert.equal(failure.find('primary composer-send').disabled,false);failure.dispose();
 const refreshed=fixture();await flush();refreshed.ctx.refresh=async()=>{throw Error('snapshot failed');};refreshed.input('accepted request');refreshed.submit();await flush();assert.equal(refreshed.find('chat-input').value,'');assert.equal(refreshed.find('chat-live-text').textContent,'Accepted. Awaiting agent response…','refresh failure does not relabel an accepted prompt as send failure');refreshed.dispose();
 const stale=fixture();await flush();stale.state.sessions.get(7).state='Done';stale.input('new work');stale.submit();await flush();await stale.poll({generation:'g1',bound:true,items:[{id:'newtool',kind:'tool_call',tool_name:'search'}],next_offset:1});stale.update();await flush();assert.notEqual(stale.find('chat-live-text').textContent,'Completed','a stale pre-send Done snapshot is not completion evidence');stale.dispose();
 const queued=fixture();await flush();queued.input('old host');queued.submit();queued.state.chatCache=new Map();await flush();assert(!queued.calls.some(c=>c.action==='prompt'),'host replacement before the queued action fences the send');queued.dispose();
 console.log('PASS composer progress: immediate provisional acknowledgement, accepted vs observed work/tool/wait/done/offline, transcript reconciliation and failure draft retry');
}
async function opaqueCursorReplay(){
 // Actual mounted mobile code, with the independently observed decreasing token pair.
 const end=5160216417328039,partial=2908416603642792;
 assert(Number.isSafeInteger(end)&&Number.isSafeInteger(partial)&&partial<end);
 const rows=(start,count)=>Array.from({length:count},(_,i)=>({id:`${start}:${i}`,kind:'assistant',text:`Record ${start}:${i}`}));
 const f=fixture({handler:async()=>({generation:'g1',agent:'opencode',bound:true,page_start:0,next_offset:end,items:rows(0,1)})});await flush();
 await f.poll({generation:'g1',agent:'opencode',bound:true,next_offset:partial,items:rows(end,128)});
 assert.deepEqual(Array.from(f.entry.pages,p=>[p.start,p.next]),[[0,end],[end,partial]]);
 // Clip inside the decreasing-token page. Exact replay must restore its missing rows.
 for(let n=0;n<7;n++)await f.poll({generation:'g1',agent:'opencode',bound:true,next_offset:partial+n+1,items:rows(partial+n,128)});
 const first=f.entry.items[0];assert.equal(first._pageStart,end);assert(first._clipped);
 f.handler=async req=>{assert.equal(req.offset,end);assert.equal(req.end_offset,partial);return {generation:'g1',bound:true,page_start:end,next_offset:partial,items:rows(end,128)};};
 f.button('Load earlier messages').action();await flush();assert.equal(f.errors.length,0);assert.equal(f.entry.items[0].id,`${end}:0`);assert(f.entry.live);
 // Then replay the old end after later appends; it remains byte-for-byte the observed end.
 f.handler=async req=>{assert.equal(req.offset,0);assert.equal(req.end_offset,end);return {generation:'g1',bound:true,page_start:0,next_offset:end,items:rows(0,1)};};
 f.button('Load earlier messages').action();await flush();assert.equal(f.entry.items[0].id,'0:0');assert.equal(f.root.descendants().find(n=>n.attrs.role==='status'&&n.children.some(c=>c.textContent==='Retry')).hidden,true);assert.equal(new Set(f.entry.items.map(i=>i.id)).size,f.entry.items.length);
 f.handler=async req=>({generation:'g1',agent:'opencode',bound:true,next_offset:req.offset,items:[]});f.button('Back to latest').action();await flush();
 // A bounded read that fails closed discards stale state and restarts from latest.
 let calls=0;f.handler=async req=>{calls++;return req.latest?{generation:'g1',agent:'opencode',bound:true,page_start:0,next_offset:end,items:rows(0,1)}:{generation:'g1',bound:false,reset:true,next_offset:0,items:[]};};
 f.update();await flush();assert.equal(calls,2);assert.equal(f.entry.items.length,1);assert.equal(f.entry.offset,end);assert.equal(f.entry.pages.length,1);f.dispose();
 // Eviction/deadline on the first earlier read must leave a usable escape to a
 // fresh latest boundary, without discarding the current paint until it succeeds.
 const stale=fixture({handler:async()=>({generation:'g1',agent:'opencode',bound:true,page_start:partial,next_offset:end,items:rows(partial,2)})});await flush();
 const old=stale.entry.items.slice();stale.handler=async()=>({generation:'g1',bound:false,reset:true,next_offset:0,items:[]});
 stale.button('Load earlier messages').action();await flush();assert.deepEqual(stale.entry.items,old);assert.equal(stale.entry.live,undefined);assert.equal(stale.button('Back to latest').hidden,false);
 stale.handler=async req=>{assert(req.latest);assert.equal(req.offset,0);return {generation:'g1',agent:'opencode',bound:true,page_start:100,next_offset:end+1,items:rows(100,2)};};
 stale.button('Back to latest').action();await flush();assert.equal(stale.entry.items[0].id,'100:0');assert.deepEqual(Array.from(stale.entry.pages,p=>[p.start,p.next]),[[100,end+1]]);stale.dispose();
 console.log('PASS opaque SQLite tokens: decreasing end-to-partial remembered, clipped replay, immutable old end, live resource reset and expired-history latest recovery');
}
async function recoveryPaint(){
 const E=5160216417328039,P=2908416603642792;
 const row=n=>({id:'stable'+n,kind:'assistant',text:'Row '+n});
 const page=(start,end,items,extra={})=>({generation:'g1',agent:'opencode',bound:true,page_start:start,next_offset:end,items,...extra});
 const metadata=f=>JSON.stringify(['pages','offset','generation','agent','bound','loaded','initialized','retainedBytes','lastSuccess'].map(k=>f.entry[k]));
 const unchanged=(f,old,cards,meta)=>{assert.deepEqual(f.entry.items,old);assert.deepEqual(f.find('chat-feed').children,cards);assert.equal(metadata(f),meta);assert.equal(f.button('Retry').parentElement.hidden,false);};
 // Exact independent regressions: unbound latest after live reset, and after expired history.
 for(const origin of ['live','history','nested-history','stale-mount','legacy']){
  for(const failure of ['unbound','throw','null']){
   const f=fixture({capabilities:{chat_tail:true},handler:async()=>page(P,E,[row(5),row(6)])});await flush();
   if(origin==='nested-history'){
    f.handler=async()=>page(P-1,P,[row(4)]);f.button('Load earlier messages').action();await flush();assert(f.entry.live);
   }
   if(origin==='history'||origin==='nested-history'){
    f.handler=async()=>page(0,0,[],{bound:false,reset:true});f.button('Load earlier messages').action();await flush();
   }
   if(origin==='legacy'){f.entry.noTail=true;f.state.snapshot.capabilities={};}
   const old=f.entry.items.slice(),cards=f.find('chat-feed').children.slice(),meta=metadata(f),live=f.entry.live;
   let n=0;f.handler=async req=>{
    n++;
    if((origin==='live'||origin==='legacy')&&!req.latest&&req.offset!==0)return page(0,0,[],{bound:false,reset:true});
    assert.equal(req.offset,0);if(origin!=='legacy')assert(req.latest);
    if(failure==='throw')throw Error('still overloaded');
    return failure==='null'?null:page(0,0,[],{bound:false,reset:false});
   };
   if(origin==='stale-mount'){f.entry.lastSuccess=1;f.remount();}
   else if(origin==='history'||origin==='nested-history')f.button('Back to latest').action();
   else f.update();
   await flush();
   if(origin==='legacy'){assert.equal(n,1);f.button('Retry').action();await flush();}
   else assert.equal(n,origin==='live'?2:1);
   // Remount creates new DOM nodes, but must retain the same transcript and page metadata.
   if(origin==='stale-mount'){assert.deepEqual(f.entry.items,old);assert.equal(f.entry.offset,E);}
   else unchanged(f,old,cards,meta);
   assert.equal(f.entry.live,live);assert.equal(f.button('Back to latest').hidden,false,origin+'/'+failure+' retains latest control');
   // Recovery intent survives remount and every Retry goes straight to a fresh tail.
   if(origin==='history'&&failure==='unbound'){f.remount();await flush();assert.deepEqual(f.entry.items,old);}
   f.handler=async req=>{assert.equal(req.offset,0);assert.equal(req.latest,origin==='legacy'?undefined:true);return page(100,E+1,[row(100)]);};
   f.button('Retry').action();await flush();assert.deepEqual(Array.from(f.entry.items,x=>x.id),['stable100']);
   assert.deepEqual(Array.from(f.entry.pages,p=>[p.start,p.next]),[[100,E+1]]);assert.equal(f.entry.offset,E+1);assert.equal(f.entry.live,null);assert.equal(f.button('Retry').parentElement.hidden,true);f.dispose();
  }
 }
 // A bound empty replacement is authoritative, and new generation data cannot merge old text.
 const empty=fixture({handler:async()=>page(P,E,[row(5)])});await flush();
 empty.handler=async req=>req.latest?page(0,0,[],{generation:'g2'}):page(0,0,[],{bound:false,reset:true});empty.update();await flush();
 assert.equal(empty.entry.items.length,0);assert.equal(empty.entry.generation,'g2');assert.equal(empty.find('chat-empty').children[0].textContent,'Enter a new message');empty.dispose();
 console.log('PASS recovery paint: exact live-reset and expired-history unbound regressions; thrown/null failures, nested history, stale remount and legacy retry preserve paint/pages until successful fresh tail; bound empty clears');
}
async function recoveryOwnership(){
 const oldPage=historyPage(200,230),fresh={...historyPage(0,1),generation:'fresh'};
 // An observed A -> B -> A identity cycle cannot revive A's pending response.
 for(const field of ['account_id','profile_id','provider','agent','cwd','groupKey','command_context']){
  for(const earlier of [false,true]){
   const f=fixture({handler:async()=>oldPage});await flush();const pending=deferred(),sess=f.state.sessions.get(7),original=sess[field];f.handler=()=>pending.promise;
   if(earlier)f.button('Load earlier messages').action();else f.update();await flush();
   sess[field]='other';f.update();assert.equal(f.entry.items.length,0,field+' identity clears old private paint');assert.equal(f.find('chat-feed').children.length,0);
   sess[field]=original;f.update();f.handler=async()=>fresh;pending.resolve(earlier?historyPage(190,200):historyPage(230,231));await flush();f.update();await flush();
   assert.deepEqual(Array.from(f.entry.items,x=>x.id),['message0']);assert.equal(f.entry.generation,'fresh');f.dispose();
  }
 }
 // Check a changed identity even when the snapshot update callback has not run yet.
 const silent=fixture({handler:async()=>oldPage});await flush();const stale=deferred();silent.handler=()=>stale.promise;silent.update();silent.state.sessions.get(7).account_id='new';stale.resolve(historyPage(230,231));await flush();assert.equal(silent.entry.items.length,0);assert.equal(silent.find('chat-feed').children.length,0);silent.dispose();
 // Reconnect epoch/transport changes fence both the initial and second (fresh-tail) await.
 for(const stage of ['live','tail','earlier'])for(const change of ['link','transport','host','session','abort','unmount','navigation']){
  const f=fixture({handler:async()=>oldPage});await flush();const old=f.entry,items=old.items.slice(),pages=JSON.stringify(old.pages),pending=deferred();
  f.handler=req=>stage==='tail'&&!req.latest?Promise.resolve({...oldPage,reset:true,items:[]}):pending.promise;
  if(stage==='earlier')f.button('Load earlier messages').action();else f.update();await flush();
  if(change==='link')f.state.connection.generation=1;
  if(change==='transport')f.ctx.transport={};
  if(change==='host'){f.state.chatCache=new Map();f.state.drafts=new Map();}
  if(change==='session')f.state.sessions.delete(7);
  if(change==='abort')f.ctx.signal={aborted:true};
  if(change==='unmount')f.dispose();
  if(change==='navigation')f.button('Back to latest').action();
  // Do not allow a deliberately resumed poll to obscure whether this response was rejected.
  f.handler=async()=>{throw Error('later retry');};
  pending.resolve(stage==='earlier'?historyPage(190,200):fresh);await flush();
  assert.deepEqual(old.items,items,stage+'/'+change+' discards stale response');assert.equal(JSON.stringify(old.pages),pages);assert.equal(old.offset,230);f.dispose();
 }
 console.log('PASS recovery ownership: identity changes clear private paint and fence A-B-A/no-update replies; link epoch, transport, host, session, abort, unmount and page navigation fence live/fresh-tail/earlier responses');
}
async function main(){
 await recoveryPaint();
 await recoveryOwnership();
 await opaqueCursorReplay();
 await composerDiscovery();
 await composerRaces();
 await composerProgress();
 await quickActions();
 await markdownFormatting();
 await orcaComposerAndMeta();
 await hiddenPrefetchAndOldDaemon();
 await tailFollow();
 await stableLatestRefresh();
 await renderBudgets();
 const cold=deferred(),recent=Array.from({length:128},(_,i)=>({id:`872:${i}`,kind:'assistant',text:`Message ${872+i}`}));
 let now=Date.now();const chatClock=class extends Date{static now(){return now;}};
 const warm=fixture({visible:false,Date:chatClock,handler:()=>cold.promise});
 assert.equal(warm.calls[0].latest,true);assert.equal(warm.find('chat-empty'),undefined,'no false empty transcript before the first reply');
 cold.resolve({generation:'g1',agent:'claude',bound:true,items:recent,page_start:872,next_offset:1000,has_earlier:true});await flush();
 assert.equal(warm.calls.length,1,'one request reaches the current tail');
 assert.equal(warm.entry.items.at(-1).text,'Message 999');
 const preparedFeed=warm.find('chat-feed'),preparedLast=preparedFeed.children.at(-1);
 warm.handler=async req=>req.latest?{generation:'g1',bound:true,page_start:744,next_offset:872,items:Array.from({length:128},(_,i)=>({id:`744:${i}`,kind:'assistant',text:`Message ${744+i}`}))}:{generation:'g1',bound:true,page_start:1000,next_offset:1000,items:[]};
 warm.setVisible(true);assert.equal(preparedFeed.scrollTop,preparedFeed.scrollHeight,'first visible frame is pinned to the latest message');await flush();
 warm.input('draft');warm.setVisible(false);warm.setVisible(true);await flush();
 assert.equal(warm.find('chat-feed'),preparedFeed);assert.equal(preparedFeed.children.at(-1),preparedLast);assert.equal(warm.find('chat-input').value,'draft');
 const warmAnchor=preparedFeed.children[0],warmCount=warm.calls.length;preparedFeed.scrollTop=0;preparedFeed.dispatchEvent(new Event('scroll'));warm.update();await flush();
 assert.equal(warm.calls.length,warmCount+1,'upward scroll loads one earlier page without a button click');
 assert.equal(warm.calls.at(-1).latest,true);assert.equal(warm.calls.at(-1).end_offset,872);
 assert.equal(warm.entry.items[0].text,'Message 744');assert.equal(preparedFeed.children[128],warmAnchor);assert.equal(preparedFeed.scrollTop,12800);
 warm.button('Back to latest').action();await flush();assert.equal(warm.entry.items[0].text,'Message 872');assert.equal(preparedFeed.scrollTop,preparedFeed.scrollHeight);
 warm.setVisible(false);now+=5000;
 warm.handler=async()=>({generation:'g1',bound:true,page_start:9872,next_offset:10000,items:[{id:'9872:0',kind:'assistant',text:'Message 9999'}]});
 warm.setVisible(true);await flush();assert.equal(warm.calls.at(-1).latest,true,'after a long stay in Term, skip the backlog in one tail request');assert.equal(warm.entry.items.at(-1).text,'Message 9999');
 const unchangedTail=preparedFeed.children.at(-1);warm.setVisible(false);now+=5000;warm.setVisible(true);await flush();assert.equal(preparedFeed.children.at(-1),unchangedTail,'an unchanged tail refresh keeps the existing message nodes');
 warm.dispose();
 console.log('PASS first response opens at latest, hidden prefetch survives mode flips, backward paging anchors and explicit latest pins');
 await historyRace();
 await automaticHistory();
 await automaticHistoryFailures();
 await inactiveHistory();
 const tools=fixture();await flush();await tools.poll({generation:'g1',bound:true,next_offset:1,items:[{id:'user',kind:'user',text:'Please review'},{id:'call',kind:'tool_call',tool_name:'Read',file_path:'/work/a.rs',text:'Large tool arguments'},{id:'result',kind:'tool_result',text:'Long tool output'},{id:'answer',kind:'assistant',text:'Here is the answer'}]});
 const run=tools.find('chat-tool-run');assert(run);assert(!run.open);assert(!tools.find('chat-tool-call').open);assert(!tools.find('chat-tool-result').open);assert.equal(tools.find('chat-feed').children.length,3);assert(run.children[0].textContent.includes('Read'));
 run.open=true;tools.find('chat-tool-call').open=true;await tools.poll({generation:'g1',bound:true,next_offset:2,items:[{id:'more',kind:'assistant',text:'More context'}]});assert.equal(tools.find('chat-tool-run'),run);assert(run.open);assert(tools.find('chat-tool-call').open);tools.dispose();
 console.log('PASS message-first chat groups adjacent tools, defaults closed and preserves disclosure on deltas');
 const failedTools=fixture();await flush();await failedTools.poll({generation:'g1',bound:true,next_offset:1,items:[{id:'failed-call',kind:'tool_call',tool_name:'Read',text:'Read the file'},{id:'failed-result',kind:'tool_result',is_error:true,text:'Permission denied'}]});
 const failedRun=failedTools.find('chat-tool-run');assert(failedRun);assert(!failedRun.open);assert(failedRun.children[0].textContent.includes('Tool error'),'a collapsed tool group must expose failures without opening it');assert(failedRun.children[0].textContent.includes('Read'));failedTools.dispose();
 console.log('PASS collapsed tool group exposes tool failure in its summary');
 let f=fixture();await flush();const pending=deferred();f.handler=async req=>req.action==='prompt'?pending.promise:{generation:'g1',items:[],next_offset:req.offset,bound:true};
 f.input('first');f.submit();await flush();f.input('second');f.submit();await flush();assert.equal(f.calls.filter(c=>c.action==='prompt').length,1);
 pending.resolve({sent:true});await flush();assert.equal(f.find('chat-input').value,'second');assert.equal(f.state.drafts.get(7),'second');
 const next=deferred();f.handler=async req=>req.action==='prompt'?next.promise:{generation:'g1',items:[],next_offset:req.offset,bound:true};f.submit();await flush();f.remount();await flush();f.input('third');f.submit();await flush();assert.equal(f.calls.filter(c=>c.action==='prompt').length,2);next.resolve({sent:true});await flush();assert.equal(f.state.drafts.get(7),'third');assert.equal(f.find('chat-input').value,'third');f.dispose();
 console.log('PASS TR-A02 delayed success preserves edits, remount draft and duplicate-submit lock');
 // The current mount must consume an older mount's successful completion, without another edit.
 f=fixture();await flush();const remounted=deferred();f.handler=async req=>req.action==='prompt'?remounted.promise:{generation:'g1',items:[],next_offset:req.offset,bound:true};
 f.input('send once');f.submit();await flush();f.remount();await flush();assert.equal(f.find('chat-input').value,'send once');remounted.resolve({sent:true});await flush();
 assert.equal(f.find('chat-input').value,'');assert.equal(f.state.drafts.has(7),false);f.submit();await flush();assert.equal(f.calls.filter(c=>c.action==='prompt').length,1);f.dispose();
 // Editing back to exactly the same text is still a newer draft and must survive completion.
 f=fixture();await flush();const edited=deferred();f.handler=async req=>req.action==='prompt'?edited.promise:{generation:'g1',items:[],next_offset:req.offset,bound:true};
 f.input('same text');f.submit();await flush();f.remount();await flush();f.input('new edit');f.input('same text');edited.resolve({sent:true});await flush();
 assert.equal(f.find('chat-input').value,'same text');assert.equal(f.state.drafts.get(7),'same text');f.dispose();
 console.log('PASS P1 composer completion clears unchanged remount, preserves even identical-text newer edits, no duplicate send');
 f=fixture();await flush();
 const q={id:'q',kind:'question',text:'Choose',multi_select:true,options:[{label:'Safe'},{label:'Fast'}]},reason={id:'r',kind:'reasoning',text:'Thought'},permission={id:'p',kind:'permission',text:'Approve?'};
 await f.poll({generation:'g1',items:[q,reason,permission],next_offset:10,bound:true});
 const card=f.find('chat-card chat-question'),custom=card.children.find(n=>n.tag==='input'),details=f.find('chat-reasoning');
 f.button('Safe').action();custom.value='keep notes';custom.dispatchEvent(new Event('input'));details.open=true;
 assert.equal(f.button('Copy answer').hidden,false);f.button('Copy answer').action();await flush();assert.equal(f.copied.at(-1),'Safe, keep notes');
 await f.poll({generation:'g1',items:[{id:'a',kind:'assistant',text:'delta'}],next_offset:20,bound:true});
 assert.equal(f.find('chat-card chat-question'),card);assert.equal(custom.value,'keep notes');assert.equal(details.open,true);assert.equal(f.button('Safe').attrs['aria-pressed'],'true');
 f.button('Confirm in Term').action();assert.equal(f.term,1);assert(!f.calls.some(c=>['approve','answer_question'].includes(c.action)));
 await f.poll({generation:'g2',items:[],next_offset:0,reset:true,bound:true});assert.equal(f.root.descendants().filter(n=>n.tag==='article').length,0);assert.equal(f.entry.items.length,0);assert.equal(f.entry.bound,true);assert.equal(f.entry.agent,'');assert.equal(f.find('chat-empty').children[0].textContent,'Enter a new message');f.dispose();
 console.log('PASS TR-A01/06/07 historical cards direct to Term, Copy answer copies draft, DOM notes/selection/expansion survive, successful empty replacement clears');
 f=fixture();await flush();f.handler=async()=>{throw new Error('history unavailable');};f.update();await flush();const status=f.root.descendants().find(n=>n.attrs.role==='status'&&n.children.some(c=>c.textContent==='Retry'));assert.equal(status.hidden,false);assert.match(status.children[0].textContent,/history unavailable/);assert.match(status.children[0].textContent,/\d\d:\d\d:\d\d/);
 f.handler=async req=>({generation:'g1',items:[],next_offset:req.offset,bound:true});f.button('Retry').action();await flush();assert.equal(status.hidden,true);f.dispose();
 console.log('PASS TR-A08 poll failure includes 24h HH:MM:SS last update, explicit retry recovers');
 // Photo picker uses ctx.el, so intercept just its file input to dispatch the actual change callback.
 f=fixture();await flush();let picker;const oldEl=f.ctx.el;f.ctx.el=(...args)=>{const n=oldEl(...args);if(n.tag==='input')picker=n;return n;};
 // mount captures el, so remount after installing instrumentation.
 f.remount();await flush();const photo1=deferred();f.prepare=()=>photo1.promise;f.button('Photo').action();picker.files=[{name:'first.jpg'}];picker.dispatchEvent(new Event('change'));await flush();assert.equal(f.entry.photo.status,'pending');f.input('with image');f.submit();await flush();assert(!f.calls.some(c=>c.action==='prompt'));
 photo1.reject(new Error('compression failed'));await flush();assert.equal(f.entry.photo.status,'error');assert(f.button('Retry photo'));f.submit();await flush();assert(!f.calls.some(c=>c.action==='prompt'));
 f.prepare=async()=>({mime:'image/jpeg',data:'image1'});f.button('Retry photo').action();await flush();assert.equal(f.entry.photo.status,'ready');
 const thumb=f.find('chat-photo-thumb');assert(thumb);assert.equal(thumb.attrs.src,'data:image/jpeg;base64,image1');
 f.submit();await flush();const sent=f.calls.find(c=>c.action==='prompt');assert.equal(sent.text,'with image');assert.equal(sent.photo.data,'image1');assert.equal(f.entry.photo,null);assert(!f.calls.some(c=>c.action==='photo'));assert.equal(f.find('chat-input').value,'');
 const stale=deferred();f.prepare=()=>stale.promise;f.button('Photo').action();picker.files=[{name:'stale.jpg'}];picker.dispatchEvent(new Event('change'));await flush();f.button('Remove photo').action();stale.resolve({mime:'image/jpeg',data:'stale'});await flush();assert.equal(f.entry.photo,null);
 // Submit rejection must retain both draft and prepared photo for retry.
 f.prepare=async()=>({mime:'image/jpeg',data:'retry-image'});f.button('Photo').action();picker.files=[{name:'retry.jpg'}];picker.dispatchEvent(new Event('change'));await flush();f.handler=async req=>{if(req.action==='prompt')throw new Error('send failed');return {generation:'g1',items:[],next_offset:req.offset};};f.input('retry draft');f.submit();await flush();assert.equal(f.state.drafts.get(7),'retry draft');assert.equal(f.entry.photo.prepared.data,'retry-image');f.dispose();
 console.log('PASS TR-A11 preparation pending/error/retry, no early send, one ordered request, remove race, failed-send retention');
 f=fixture();await flush();await f.poll({generation:'g1',items:[{id:'tool',kind:'tool_call',file_path:'/work/src/a.rs',text:'read'}],next_offset:1,bound:true});f.handler=async req=>req.action==='git'?{patch:'',diff_status:'no_current_diff'}:{};f.button('Current diff').action();await flush();assert.match(f.sheets[0].body.children[0].textContent,/historical tool record/);assert.equal(f.calls.at(-1).path,'src/a.rs');f.dispose();
 console.log('PASS TR-A12 current empty diff has historical-context explanation');
 f=fixture();await flush();const items=Array.from({length:1050},(_,i)=>({id:`row${i}`,kind:'assistant',text:`Message ${i}`}));
 f.handler=async req=>{const end=Math.min(items.length,req.offset+128,req.end_offset??Infinity);return {generation:'g1',items:items.slice(req.offset,end),next_offset:end,bound:true};};
 for(let i=0;i<9;i++){f.update();await flush();}assert.equal(f.entry.items.length,1000);assert.equal(f.entry.items[0].id,'row50');assert.equal(f.button('Load earlier messages').hidden,false);
 const feed=f.find('chat-feed'),anchor=feed.children[0];feed.scrollTop=0;const oldTop=anchor.offsetTop;f.button('Load earlier messages').action();await flush();assert.equal(f.entry.items.length,1000);assert.equal(f.entry.items[0].id,'row0');assert.equal(f.find('chat-feed').children[50],anchor);assert.equal(feed.scrollTop,anchor.offsetTop-oldTop);assert(f.entry.live);const count=f.calls.length;f.update();await flush();assert.equal(f.calls.length,count);f.button('Back to latest').action();await flush();assert.equal(f.entry.items[0].id,'row50');assert.equal(f.entry.items.at(-1).id,'row1049');assert.equal(f.entry.offset,1050);f.dispose();
 // Repeated previous-page reads must use prior offsets, not merely replay the first clipped page.
 f=fixture();await flush();const longer=Array.from({length:1350},(_,i)=>({id:`long${i}`,kind:'assistant',text:`Older ${i}`}));
 const longPage=async req=>{const end=Math.min(longer.length,req.offset+128,req.end_offset??Infinity);return {generation:'g1',items:longer.slice(req.offset,end),next_offset:end,bound:true};};f.handler=longPage;
 for(let i=0;i<11;i++){f.update();await flush();}assert.equal(f.entry.items[0].id,'long350');
 f.button('Load earlier messages').action();await flush();assert.equal(f.entry.items[0].id,'long256');
 f.button('Load earlier messages').action();await flush();assert.equal(f.entry.items[0].id,'long128');assert.equal(f.calls.at(-1).offset,128);
 const before=f.entry.items[0];f.handler=async()=>{throw new Error('earlier page failed');};f.button('Load earlier messages').action();await flush();assert.equal(f.entry.items[0],before);assert.equal(f.button('Load earlier messages').disabled,false);f.handler=longPage;f.button('Load earlier messages').action();await flush();assert.equal(f.entry.items[0].id,'long0');assert.equal(f.entry.items.at(-1).id,'long999');assert.equal(f.entry.items.length,1000);assert.equal(f.button('Load earlier messages').hidden,true);f.dispose();
 console.log('PASS TR-A14 visible 1000 cap, real earlier RPC, stable anchor, history pause and latest restore');
 // Realistic byte offsets and page-local IDs: every append was observed as a one-item page.
 // A later unbounded replay would return 128 items with different IDs for the same records.
 f=fixture();await flush();const records=Array.from({length:1050},(_,i)=>({kind:'assistant',text:i===48||i===49?'Identical text':`Message ${i}`}));
 const offsets=[0];for(const record of records)offsets.push(offsets.at(-1)+Buffer.byteLength(JSON.stringify(record)+'\n'));
 let total=0;f.handler=async req=>{
  const begin=offsets.indexOf(req.offset);assert(begin>=0,'request starts at a recorded byte boundary');
  const end=Math.min(total,begin+128,req.end_offset===undefined?total:offsets.indexOf(req.end_offset));
  assert(end>=begin,'replay end is a recorded byte boundary');
  return {generation:'g1',items:records.slice(begin,end).map((item,i)=>({...item,id:`${req.offset}:${i}`})),next_offset:offsets[end],bound:true};
 };
 for(total=1;total<=1050;total++){f.update();await flush();}
 const smallIds=(start)=>Array.from({length:1000},(_,i)=>`${offsets[start+i]}:0`);
 assert.deepEqual(Array.from(f.entry.items,item=>item.id),smallIds(50));assert.equal(f.entry.items[0]._clipped,undefined);
 const firstCard=f.find('chat-feed').children[0];f.find('chat-feed').scrollTop=0;
 for(let start=49;start>=0;start--){
  f.button('Load earlier messages').action();await flush();
  assert.equal(f.calls.at(-1).offset,offsets[start]);assert.equal(f.calls.at(-1).end_offset,offsets[start+1]);
  assert.deepEqual(Array.from(f.entry.items,item=>item.id),smallIds(start));assert.equal(f.entry.items.length,1000);assert.equal(f.errors.length,0);
  if(start===49){assert.equal(f.find('chat-feed').children[1],firstCard);assert.equal(f.find('chat-feed').scrollTop,100);}
  if(start===48){assert.equal(f.entry.items[0].text,'Identical text');assert.equal(f.entry.items[1].text,'Identical text');assert.notEqual(f.entry.items[0].id,f.entry.items[1].id);}
 }
 assert.equal(f.button('Load earlier messages').hidden,true);assert.equal(new Set(f.entry.items.map(item=>item.id)).size,1000);
 f.button('Back to latest').action();await flush();assert.deepEqual(Array.from(f.entry.items,item=>item.id),smallIds(50));assert.equal(f.entry.offset,offsets[1050]);f.dispose();
 console.log('PASS P1 1050 one-item append pages replay exact byte ends, 50 earlier reads stay unique/chronological, identical text retained');
 f=fixture();await flush();const previousHost=deferred();f.handler=async req=>req.action==='prompt'?previousHost.promise:{generation:'old',items:[],next_offset:0};f.input('same text');f.submit();await flush();
 f.state.drafts=new Map([[7,'same text']]);f.state.chatCache=new Map();f.remount();await flush();previousHost.resolve({sent:true});await flush();assert.equal(f.state.drafts.get(7),'same text');assert.equal(f.find('chat-input').value,'same text');f.dispose();
 console.log('PASS late previous-host prompt completion cannot erase the new host draft with an identical session ID/text');
}
main().catch(error=>{console.error(error);process.exitCode=1;});
