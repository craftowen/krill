// Chat failure-path regressions. No daemon, user session, network or browser required.
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
class Element extends EventTarget{
 constructor(tag,text,cls){super();this.tag=tag;this.textContent=text;this.className=cls;this.children=[];this.dataset={};this.attrs={};this.scrollTop=0;this.clientHeight=100;this.value='';}
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
function fixture(){
 const buttons=[],calls=[],sheets=[],errors=[],tasks=[],timers=new Map();let id=0,term=0;
 const sandbox={Event,Date,setTimeout:(fn)=>{timers.set(++id,fn);return id;},clearTimeout:id=>timers.delete(id),KrillMobile:{}};
 vm.createContext(sandbox);vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/chat.js'),'utf8'),sandbox);
 const state={sessions:new Map([[7,{id:7,state:'Waiting',cwd:'/work'}]]),connection:{phase:'ready'},drafts:new Map()};
 const root=new Element('root'),el=(...args)=>new Element(...args);
 let handler=async request=>request.action==='chat_poll'?{generation:'g1',agent:'claude',bound:true,items:[],next_offset:request.offset}:{sent:true};
 let prepare=async()=>({mime:'image/jpeg',data:'encoded'});
 const ctx={el,state,button(label,action,cls){const b=el('button',label,cls);b.action=action;buttons.push(b);return b;},rpc:async request=>{calls.push(request);return handler(request);},photo:(...args)=>prepare(...args),act(fn){const p=Promise.resolve().then(fn).catch(e=>errors.push(e));tasks.push(p);return p;},refresh:async()=>{},sheet(title,_,__,render){const body=el('body');render(body);sheets.push({title,body});}};
 let view=sandbox.KrillMobile.chat.mount(root,{ctx,sessionId:7,onSwitchTerm:()=>term++});
 const find=cls=>root.descendants().find(n=>n.className===cls),button=label=>root.descendants().find(n=>n.tag==='button'&&n.textContent===label);
 return {root,state,ctx,calls,buttons,sheets,errors,tasks,find,button,get term(){return term;},get entry(){return state.chatCache.get(7);},set handler(h){handler=h;},set prepare(h){prepare=h;},update(){view.update();},remount(){view.unmount();view=sandbox.KrillMobile.chat.mount(root,{ctx,sessionId:7,onSwitchTerm:()=>term++});},dispose(){view.unmount();},input(text){const input=find('chat-input');input.value=text;input.dispatchEvent(new Event('input'));},submit(){find('chat-composer').dispatchEvent(new Event('submit',{cancelable:true}));},async poll(data){handler=async req=>req.action==='chat_poll'?data:{sent:true};view.update();await flush();}};
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
async function main(){
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
 f.button('Safe').action();custom.value='keep notes';details.open=true;
 await f.poll({generation:'g1',items:[{id:'a',kind:'assistant',text:'delta'}],next_offset:20,bound:true});
 assert.equal(f.find('chat-card chat-question'),card);assert.equal(custom.value,'keep notes');assert.equal(details.open,true);assert.equal(f.button('Safe').attrs['aria-pressed'],'true');
 f.button('Confirm in Term').action();assert.equal(f.term,1);assert(!f.calls.some(c=>['approve','answer_question'].includes(c.action)));
 await f.poll({generation:'g1',items:[],next_offset:3,reset:true,bound:false});assert.equal(f.root.descendants().filter(n=>n.tag==='article').length,0);assert.equal(f.entry.items.length,0);assert.equal(f.entry.bound,false);assert.equal(f.entry.agent,'');f.dispose();
 console.log('PASS TR-A01/06/07 historical cards direct to Term, DOM notes/selection/expansion survive, empty reset clears immediately');
 f=fixture();await flush();f.handler=async()=>{throw new Error('history unavailable');};f.update();await flush();const status=f.root.descendants().find(n=>n.attrs.role==='status'&&n.children.some(c=>c.textContent==='Retry'));assert.equal(status.hidden,false);assert.match(status.children[0].textContent,/history unavailable/);
 f.handler=async req=>({generation:'g1',items:[],next_offset:req.offset,bound:true});f.button('Retry').action();await flush();assert.equal(status.hidden,true);f.dispose();
 console.log('PASS TR-A08 poll failure includes last update, explicit retry recovers');
 // Photo picker uses ctx.el, so intercept just its file input to dispatch the actual change callback.
 f=fixture();await flush();let picker;const oldEl=f.ctx.el;f.ctx.el=(...args)=>{const n=oldEl(...args);if(n.tag==='input')picker=n;return n;};
 // mount captures el, so remount after installing instrumentation.
 f.remount();await flush();const photo1=deferred();f.prepare=()=>photo1.promise;f.button('Photo').action();picker.files=[{name:'first.jpg'}];picker.dispatchEvent(new Event('change'));await flush();assert.equal(f.entry.photo.status,'pending');f.input('with image');f.submit();await flush();assert(!f.calls.some(c=>c.action==='prompt'));
 photo1.reject(new Error('compression failed'));await flush();assert.equal(f.entry.photo.status,'error');assert(f.button('Retry photo'));f.submit();await flush();assert(!f.calls.some(c=>c.action==='prompt'));
 f.prepare=async()=>({mime:'image/jpeg',data:'image1'});f.button('Retry photo').action();await flush();assert.equal(f.entry.photo.status,'ready');f.submit();await flush();const sent=f.calls.find(c=>c.action==='prompt');assert.equal(sent.text,'with image');assert.equal(sent.photo.data,'image1');assert.equal(f.entry.photo,null);assert(!f.calls.some(c=>c.action==='photo'));assert.equal(f.find('chat-input').value,'');
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
