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
 const state={sessions:new Map([[7,{id:7,state:'Waiting',cwd:'/work'}]]),connection:{phase:options.phase||'ready'},drafts:new Map()};
 const root=new Element('root'),el=(...args)=>new Element(...args);
 let handler=options.handler|| (async request=>request.action==='chat_poll'?{generation:'g1',agent:'claude',bound:true,items:[],next_offset:request.offset}:{sent:true});
 let prepare=async()=>({mime:'image/jpeg',data:'encoded'});
 const ctx={el,state,button(label,action,cls){const b=el('button',label,cls);b.action=action;buttons.push(b);return b;},rpc:async request=>{calls.push(request);return handler(request);},photo:(...args)=>prepare(...args),act(fn){const p=Promise.resolve().then(fn).catch(e=>errors.push(e));tasks.push(p);return p;},refresh:async()=>{},sheet(title,_,__,render){const body=el('body');render(body);sheets.push({title,body});}};
 let view=sandbox.KrillMobile.chat.mount(root,{ctx,sessionId:7,visible:options.visible,onQuickActions:options.onQuickActions,onSwitchTerm:()=>term++});
 const find=cls=>root.descendants().find(n=>n.className===cls),button=label=>root.descendants().find(n=>n.tag==='button'&&n.textContent===label);
 return {insertText:text=>view.insertText(text),root,state,ctx,calls,buttons,sheets,errors,tasks,find,button,timers,doc,copied,M:sandbox.KrillMobile,get term(){return term;},get entry(){return state.chatCache.get(7);},set handler(h){handler=h;},set prepare(h){prepare=h;},update(){view.update();},setVisible(v){view.setVisible(v);},remount(){view.unmount();view=sandbox.KrillMobile.chat.mount(root,{ctx,sessionId:7,onSwitchTerm:()=>term++});},dispose(){view.unmount();},input(text){const input=find('chat-input');input.value=text;input.dispatchEvent(new Event('input'));},submit(){find('chat-composer').dispatchEvent(new Event('submit',{cancelable:true}));},async poll(data){handler=async req=>req.action==='chat_poll'?data:{sent:true};view.update();await flush();}};
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
 const entries=[];f.ctx.sheet=(title,items)=>entries.push(...items);
 const model=f.buttons.find(b=>b.className==='composer-pill');assert.equal(model.hidden,false);model.action();
 assert.deepEqual(Array.from(entries,e=>e.label),['Default (recommended)','Opus','Sonnet','Haiku']);
 entries[2].action();await flush();assert.deepEqual({...f.calls.at(-1)},{action:'prompt',session:7,text:'/model sonnet'});f.dispose();
 const codex=fixture();await flush();await codex.poll({generation:'g1',agent:'codex',bound:true,items:[],next_offset:0});
 codex.buttons.find(b=>b.className==='composer-pill').action();await flush();
 assert.deepEqual({...codex.calls.at(-1)},{action:'prompt',session:7,text:'/model'});assert.equal(codex.term,1,'Codex picks its model in the terminal picker');codex.dispose();
 const other=fixture();await flush();await other.poll({generation:'g1',agent:'agy',bound:true,items:[],next_offset:0});
 assert.equal(other.buttons.find(b=>b.className==='composer-pill').hidden,true,'agents without a known /model command get no Model button');other.dispose();
 console.log('PASS Orca composer: copy/time under messages, Model sends /model to Claude or opens the Codex picker in Term');
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
async function main(){
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
 preparedFeed.scrollTop=0;preparedFeed.dispatchEvent(new Event('scroll'));warm.update();await flush();assert.equal(preparedFeed.scrollTop,0,'live poll respects reading position');
 const warmAnchor=preparedFeed.children[0];warm.button('Load earlier messages').action();await flush();
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
 await f.poll({generation:'g1',items:[],next_offset:3,reset:true,bound:false});assert.equal(f.root.descendants().filter(n=>n.tag==='article').length,0);assert.equal(f.entry.items.length,0);assert.equal(f.entry.bound,false);assert.equal(f.entry.agent,'');assert.equal(f.find('chat-empty').children[0].textContent,'Enter a new message');f.dispose();
 console.log('PASS TR-A01/06/07 historical cards direct to Term, Copy answer copies draft, DOM notes/selection/expansion survive, empty reset clears immediately');
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
