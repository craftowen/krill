'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),http=require('node:http');
const {execFile}=require('node:child_process'),run=require('node:util').promisify(execFile);
async function main(){
 const assets=path.join(__dirname,'../src/mobile/assets'),session=`krill-chat-scroll-${process.pid}`;
 const browser=async(...args)=>(await run('agent-browser',['--session',session,...args],{timeout:45000})).stdout.trim();
 const fixture=`
 const el=(tag,text,cls)=>{const n=document.createElement(tag);if(text!==undefined)n.textContent=text;if(cls)n.className=cls;return n;};
 const state={sessions:new Map([[7,{id:7,state:'Working'}]]),connection:{phase:'ready'},drafts:new Map(),snapshot:{capabilities:{chat_tail:true}}};
 window.calls=[];window.held=[];window.holdEarlier=false;
 const records=Array.from({length:300},(_,i)=>({id:'m'+i,kind:'assistant',text:'Message '+i+' '+('Readable body '.repeat(20))}));
 const ctx={el,state,button:(text,fn,cls)=>{const n=el('button',text,cls);n.type='button';n.onclick=fn;return n;},rpc:async q=>{
  calls.push(q);if(q.action!=='chat_poll')return {};let start=q.latest?Math.max(0,(q.end_offset??300)-30):q.offset,end=q.end_offset??300;
  if(q.end_offset!==undefined&&holdEarlier)await new Promise(r=>held.push(r));
  return {generation:'g',agent:'claude',bound:true,page_start:start,next_offset:end,items:records.slice(start,end)};
 },sheet:()=>{}};
 window.view=KrillMobile.chat.mount(document.querySelector('main'),{ctx,sessionId:7});window.feed=document.querySelector('.chat-feed');
 window.metrics=()=>({top:feed.scrollTop,height:feed.scrollHeight,client:feed.clientHeight,first:state.chatCache.get(7).items[0].id,earlier:calls.filter(q=>q.end_offset!==undefined).length});
 `;
 const html='<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/app.css"><div id="mobile-app"><div class="session screen"><main class="terminal-mount"></main></div></div><script src="/screens/chat.js"></script><script src="/fixture.js"></script>';
 const server=http.createServer((req,res)=>{if(req.url==='/favicon.ico'){res.writeHead(204);res.end();return;}res.setHeader('Content-Type',req.url==='/app.css'?'text/css':req.url.endsWith('.js')?'text/javascript':'text/html');res.end(req.url==='/fixture.js'?fixture:req.url==='/'?html:fs.readFileSync(path.join(assets,req.url)));});
 let socket;const pending=new Map();let nextId=0;
 try{
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  await browser('set','viewport','390','844');await browser('open',`http://127.0.0.1:${server.address().port}/`);
  const evaluate=async code=>JSON.parse(await browser('eval',`(async()=>{${code}})()`));
  await browser('wait','--fn','window.feed&&feed.scrollHeight>feed.clientHeight');
  const before=await evaluate('return metrics();');
  assert(before.earlier>0,'opening chat prefetches the preceding page before the reader scrolls');
  await evaluate('window.anchor=feed.children[1];feed.scrollTop=anchor.offsetTop-120;window.anchorTop=anchor.getBoundingClientRect().top;feed.dispatchEvent(new Event("scroll"));return true;');
  await browser('wait','--fn','metrics().first==="m240"');
  const after=await evaluate('return {...metrics(),anchorTop:anchor.getBoundingClientRect().top,expected:anchorTop};');
  assert(after.top>0,'prepend preserves room above the reader');
  assert(Math.abs(after.anchorTop-after.expected)<=1,'the visible message stays at the same viewport position');
  assert(after.earlier>=2,'next preceding page is prefetched after consuming the first');
  socket=new WebSocket(await browser('get','cdp-url'));await new Promise((resolve,reject)=>{socket.onopen=resolve;socket.onerror=reject;});
  socket.onmessage=e=>{const message=JSON.parse(e.data),p=pending.get(message.id);if(!p)return;pending.delete(message.id);clearTimeout(p.timer);message.error?p.reject(Error(JSON.stringify(message.error))):p.resolve(message.result);};
  const send=(method,params={},sessionId)=>new Promise((resolve,reject)=>{const id=++nextId,timer=setTimeout(()=>{pending.delete(id);reject(Error('CDP timeout'));},15000);pending.set(id,{resolve,reject,timer});socket.send(JSON.stringify({id,method,params,sessionId}));});
  const {targetInfos}=await send('Target.getTargets'),target=targetInfos.find(t=>t.type==='page'&&t.url.startsWith('http://127.0.0.1:'+server.address().port));assert(target);
  const {sessionId}=await send('Target.attachToTarget',{targetId:target.targetId,flatten:true}),call=(method,params)=>send(method,params,sessionId);
  await call('Emulation.setTouchEmulationEnabled',{enabled:true,maxTouchPoints:1});
  const point=await evaluate('feed.scrollTop=1200;window.trustedMoves=0;feed.addEventListener("touchmove",e=>{if(e.isTrusted)trustedMoves++;},{passive:true});const r=feed.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+100};');
  await call('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[point]});
  for(let distance=10;distance<=400;distance+=10){await new Promise(resolve=>setTimeout(resolve,15));await call('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:point.x,y:point.y+distance}]});}
  await call('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});
  await browser('wait','--fn','metrics().first==="m210"');
  const touched=await evaluate('return {...metrics(),trustedMoves};');assert(touched.trustedMoves>0,'regression drives trusted mobile touch');assert(touched.top>1000,'earlier messages prepend before reaching the top');
  await evaluate('view.unmount();state.chatCache.clear();calls.length=0;records.forEach((record,i)=>{record.kind=i>=270?"tool_call":"assistant";record.tool_name="Read";});window.view=KrillMobile.chat.mount(document.querySelector("main"),{ctx,sessionId:7});window.feed=document.querySelector(".chat-feed");return true;');
  await browser('wait','--fn','metrics().first==="m240"&&feed.scrollHeight>feed.clientHeight');
  const filled=await evaluate('return {...metrics(),historical:!!state.chatCache.get(7).live};');assert.equal(filled.historical,false,'short-feed fill keeps live polling');assert(filled.top>0,'short-feed fill stays at latest');
  await evaluate('view.unmount();state.chatCache.clear();calls.length=0;records.forEach(record=>record.kind="tool_call");window.view=KrillMobile.chat.mount(document.querySelector("main"),{ctx,sessionId:7});window.feed=document.querySelector(".chat-feed");return true;');
  await browser('wait','--fn','metrics().first==="m150"');
  const collapsed=await evaluate('const r=feed.getBoundingClientRect();return {...metrics(),point:{x:r.x+r.width/2,y:r.y+120}};');assert.equal(collapsed.height,collapsed.client,'collapsed groups leave no native scroll range');
  for(const expected of ['m30','m0']){
   await call('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[collapsed.point]});
   for(let distance=4;distance<=100;distance+=4){await new Promise(resolve=>setTimeout(resolve,15));await call('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:collapsed.point.x,y:collapsed.point.y+distance}]});}
   await call('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});await browser('wait','--fn',`metrics().first==="${expected}"`);
  }
  await evaluate(`view.unmount();state.chatCache.clear();records.forEach(record=>record.kind="assistant");state.snapshot.capabilities={chat_tail:true,chat_composer:true,composer_models:true};window.liveMeta={agent:"codex",model:"gpt-long-model-name-for-small-mobile",effort:"xhigh",context:{percent:37},effort_supported:true,models:[{id:"gpt-long-model-name-for-small-mobile",label:"Current model",efforts:["high","xhigh"]},{id:"other-model",label:"Other model",efforts:["low","high"]}],efforts:[]};window.baseRpc=ctx.rpc;ctx.rpc=async q=>{if(q.action==="chat_composer")return structuredClone(liveMeta);if(q.action==="composer_models")return {supported:true,current:liveMeta.model,items:liveMeta.models};if(q.action==="set_model"){liveMeta.model=q.model;liveMeta.effort=q.effort;calls.push(q);return {applied:true,model:q.model,effort:q.effort};}const res=await baseRpc(q);return {...res,agent:"codex"};};window.view=KrillMobile.chat.mount(document.querySelector("main"),{ctx,sessionId:7,onQuickActions:()=>{}});window.feed=document.querySelector(".chat-feed");return true;`);
  await browser('wait','--fn','document.querySelector(".composer-model-effort").textContent.includes("xhigh")');
  const evidence=path.resolve(process.argv[2]||`/tmp/krill-release-chat-ui-${process.pid}`);fs.mkdirSync(evidence,{recursive:true});
  for(const width of [320,390]){await browser('set','viewport',String(width),'844');const bounds=await evaluate('const row=document.querySelector(".chat-composer-status").getBoundingClientRect(),effort=document.querySelector(".composer-model-effort").getBoundingClientRect(),actions=document.querySelector(".chat-composer-actions").getBoundingClientRect();return {width:innerWidth,scroll:document.documentElement.scrollWidth,row:{left:row.left,right:row.right,bottom:row.bottom},effort:{left:effort.left,right:effort.right},actions:{top:actions.top,right:actions.right}};');assert.equal(bounds.scroll,width,'composer has no horizontal overflow');assert(bounds.effort.right<=bounds.row.right&&bounds.effort.left>=bounds.row.left,'effort remains visible alongside the truncated model');assert(bounds.actions.top>=bounds.row.bottom,'actions have a reserved second row');await browser('screenshot',path.join(evidence,`composer-${width}.png`));}
  await browser('click','.composer-pill');await browser('wait','--fn','document.querySelectorAll(".chat-composer-option").length===3');await browser('click','#chat-option-7-1');await browser('wait','--fn','document.querySelector(".chat-composer-options")===null||document.querySelectorAll(".chat-composer-option").length===2');await browser('click','#chat-option-7-1');await browser('wait','--fn','calls.some(q=>q.action==="set_model"&&q.model==="other-model"&&q.effort==="high")');
  console.log('PASS Chromium 390x844 chat prefetch, automatic pagination, stable reader anchor, trusted touch and bounded collapsed-history gesture continuation');
 }finally{for(const p of pending.values()){clearTimeout(p.timer);p.reject(Error('CDP closed'));}socket?.close();await browser('close').catch(()=>{});await new Promise(resolve=>server.close(resolve));}
}
main().catch(error=>{console.error(error);process.exitCode=1;});
