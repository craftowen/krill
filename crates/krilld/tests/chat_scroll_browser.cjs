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
  await evaluate(`view.unmount();state.chatCache.clear();records.forEach(record=>record.kind="assistant");state.snapshot.capabilities={chat_tail:true,chat_composer:true,composer_models:true};window.liveMeta={agent:"codex",model:"gpt-long-model-name-for-small-mobile",effort:"xhigh",context:{percent:37},effort_supported:true,models:[{id:"gpt-long-model-name-for-small-mobile",label:"Current model",efforts:["high","xhigh"]},{id:"other-model",label:"Other model",efforts:["low","high"]}],efforts:[]};window.baseRpc=ctx.rpc;ctx.rpc=async q=>{if(q.action==="chat_composer")return structuredClone(liveMeta);if(q.action==="composer_models")return {supported:true,current:liveMeta.model,items:liveMeta.models};if(q.action==="set_model"){liveMeta.model=q.model;liveMeta.effort=q.effort;calls.push(q);return {applied:true,model:q.model,effort:q.effort};}const res=await baseRpc(q);return {...res,agent:"codex"};};window.view=KrillMobile.chat.mount(document.querySelector("main"),{ctx,sessionId:7});window.feed=document.querySelector(".chat-feed");return true;`);
  await browser('wait','--fn','document.querySelector(".composer-model-effort")?.dataset?.effort==="xhigh"&&document.querySelector(".composer-model-effort")?.textContent==="XHigh"');
  const evidence=path.resolve(process.argv[2]||`/tmp/krill-release-chat-ui-${process.pid}`);fs.mkdirSync(evidence,{recursive:true});
  for(const width of [320,390]){await browser('set','viewport',String(width),'844');const bounds=await evaluate('const box=selector=>document.querySelector(selector).getBoundingClientRect(),pill=box(".composer-pill"),effort=box(".composer-model-effort"),send=box(".composer-send"),input=box(".chat-input"),attach=box(".composer-icon"),meter=box(".composer-context"),card=box(".chat-composer-box"),row=box(".chat-status-row"),hit=(x,y)=>!!document.elementFromPoint(x,y)?.closest(".composer-pill"),hitPhoto=(x,y)=>!!document.elementFromPoint(x,y)?.closest(".composer-icon"),inRow=b=>b.top>=row.top&&b.bottom<=row.bottom,inCard=b=>b.left>=card.left&&b.right<=card.right&&b.top>=card.top&&b.bottom<=card.bottom;return {width:innerWidth,scroll:document.documentElement.scrollWidth,pill:{left:pill.left,right:pill.right,top:pill.top,height:pill.height},effort:{left:effort.left,right:effort.right},send:{left:send.left},order:attach.right<=pill.left&&pill.right<=meter.left&&meter.right<=send.left&&[attach,pill,meter,send].every(inRow)&&[input,row].every(inCard),input:{left:input.left,right:input.right,bottom:input.bottom,height:input.height},card:{bottom:card.bottom},row:{height:row.height,top:row.top},chipHits:[pill.top+pill.height/2].map(y=>hit(pill.left+pill.width/2,y)),aboveChip:hit(pill.left+pill.width/2,input.bottom-2),photoHits:[[attach.left+attach.width/2,attach.top+attach.height/2]].map(p=>hitPhoto(...p))};');assert.equal(bounds.scroll,width,'composer has no horizontal overflow');assert(bounds.effort.right<=bounds.pill.right&&bounds.effort.left>=bounds.pill.left,'effort remains visible alongside the truncated model');assert(bounds.input.bottom<=bounds.row.top,'input sits above unified status row');assert(bounds.order,'photo, model, context and send share the control row inside unified card');assert(bounds.photoHits.every(Boolean),'photo hit area spans the touch target');assert(bounds.row.height>=38,'control row keeps Orca 38px control height');assert(bounds.chipHits.every(Boolean),'chip hit area spans the touch target');assert(!bounds.aboveChip,'chip hit area stops short of the input row');await browser('screenshot',path.join(evidence,`composer-${width}.png`));}
  await evaluate('document.querySelector(".chat-input").focus();return true;');const typing=await evaluate('const row=document.querySelector(".chat-status-row"),before=feed.clientHeight;state.sessions.get(7).state="Done";view.update();const idle=getComputedStyle(row).display;state.sessions.get(7).state="Waiting";view.update();const waiting=getComputedStyle(row).display;state.sessions.get(7).state="Done";view.update();document.activeElement.blur();return {idle,waiting};');assert.equal(typing.idle,'flex','the control row stays while typing');assert.equal(typing.waiting,'flex','a waiting agent keeps the control row while typing');
  await browser('click','.composer-pill');await browser('wait','--fn','document.querySelector(".chat-composer-suggestions")?.dataset?.kind==="models"&&document.querySelectorAll(".chat-model-row").length>=2');await browser('click','#chat-option-7-1');await browser('wait','--fn','calls.some(q=>q.action==="set_model"&&q.model==="other-model")');
  // Mobile 390x844 voice dictation stability + immediate scroll-to-bottom & optimistic focus on submit.
  const appSource=fs.readFileSync(path.join(assets,'app.js'),'utf8'),busyExpr=appSource.includes('editable(document.activeElement)||');
  assert(busyExpr,'app.js treats any focused editable element as update-busy even before first dictated word');
  const dictation=await evaluate(`
   const input=document.querySelector(".chat-input");
   feed.scrollTop=feed.scrollHeight;feed.dispatchEvent(new Event("scroll"));
   input.focus();
   const editable=node=>node?.isContentEditable||node?.matches?.("textarea,input:not([type=button]):not([type=submit]):not([type=file]):not([type=checkbox]):not([type=radio]):not([type=range]),select");
   const focusedEmptyBusy=editable(document.activeElement);
   let heightWrites=0,rawHeight=input.style.height;
   Object.defineProperty(input.style,"height",{configurable:true,get:()=>rawHeight,set:v=>{heightWrites++;rawHeight=v;input.setAttribute("style",v?"height:"+v:"");}});
   input.value="첫 번째 음성 문장입니다\\n두 번째 음성 문장입니다\\n세 번째 음성 문장입니다\\n네 번째 음성 문장입니다\\n다섯 번째 음성 문장입니다\\n여섯 번째 음성 문장입니다";
   input.dispatchEvent(new Event("input",{bubbles:true}));
   const afterDictationWrites=heightWrites;
   view.update();view.update();view.update();
   const idleWrites=heightWrites-afterDictationWrites;
   const active=document.activeElement===input,value=input.value,pinnedGap=feed.scrollHeight-feed.scrollTop-feed.clientHeight;
   delete input.style.height;
   input.value="음성 커밋 누락 방지 문장";
   input.dispatchEvent(new Event("blur"));
   const savedDraft=state.drafts.get(7);
   input.value="";input.dispatchEvent(new Event("input",{bubbles:true}));
   return {focusedEmptyBusy,idleWrites,active,valueLen:value.length,pinnedGap,savedDraft};
  `);
  assert.equal(dictation.focusedEmptyBusy,true,'focused empty chat input blocks background reload during initial speech');
  assert.equal(dictation.idleWrites,0,'1.5s background update ticks do not reset textarea height during dictation');
  assert.equal(dictation.active,true,'chat input retains focus across background updates');
  assert(dictation.pinnedGap<=2,'growing multiline dictation keeps latest chat feed pinned above composer');
  assert.equal(dictation.savedDraft,'음성 커밋 누락 방지 문장','blur commits native dictated text to drafts');
  await evaluate(`
   feed.scrollTop=200;feed.dispatchEvent(new Event("scroll"));
   window.promptGate=null;window.pollGate=null;
   const prevRpc=ctx.rpc;
   ctx.rpc=async q=>{
    if(q.action==="prompt"){calls.push(q);await new Promise(r=>window.promptGate=r);records.push({id:"u300",kind:"user",text:q.text},{id:"a301",kind:"assistant",text:"바로 확인했습니다."});return {sent:true};}
    if(q.action==="chat_poll"&&window.holdNextPoll){window.holdNextPoll=false;await new Promise(r=>window.pollGate=r);return {generation:"g",agent:"codex",bound:true,page_start:Math.max(0,records.length-30),next_offset:records.length,items:records.slice(-30)};}
    return prevRpc(q);
   };
   window.holdNextPoll=true;
   const input=document.querySelector(".chat-input");
   input.focus();input.value="모바일 채팅 최하단 즉시 이동 테스트";input.dispatchEvent(new Event("input",{bubbles:true}));
   document.querySelector(".composer-send").click();
   return true;
  `);
  const sendingSnap=await evaluate(`
   const opt=document.querySelector(".chat-optimistic"),feedBox=feed.getBoundingClientRect(),optBox=opt?.getBoundingClientRect(),compBox=document.querySelector(".chat-composer").getBoundingClientRect();
   return {hasOpt:!!opt,phase:opt?.dataset.phase,text:opt?.innerText||"",gap:feed.scrollHeight-feed.scrollTop-feed.clientHeight,visible:!!optBox&&optBox.bottom<=compBox.top+1.5&&optBox.top>=feedBox.top};
  `);
  assert.equal(sendingSnap.hasOpt,true,'submitted message appears immediately in chat feed');
  assert.equal(sendingSnap.phase,'sending');
  assert(sendingSnap.text.includes('모바일 채팅 최하단 즉시 이동 테스트'));
  assert(sendingSnap.gap<=2,'scrolled-up chat feed jumps immediately to bottom on submit');
  assert.equal(sendingSnap.visible,true,'newly submitted message is immediately visible above composer');
  await evaluate('window.promptGate();return true;');
  await browser('wait','--fn','document.querySelector(".chat-input").value===""&&document.querySelector(".chat-optimistic")?.dataset.phase==="accepted"');
  const acceptedSnap=await evaluate('return {hasOpt:!!document.querySelector(".chat-optimistic"),gap:feed.scrollHeight-feed.scrollTop-feed.clientHeight};');
  assert.equal(acceptedSnap.hasOpt,true,'message does not vanish after input clears while awaiting transcript poll');
  assert(acceptedSnap.gap<=2,'feed stays pinned at bottom when composer clears');
  await evaluate('window.pollGate();return true;');
  await browser('wait','--fn','!document.querySelector(".chat-optimistic")&&Array.from(document.querySelectorAll(".chat-user")).some(n=>n.innerText.includes("모바일 채팅 최하단 즉시 이동 테스트"))');
  const settledSnap=await evaluate('const users=Array.from(document.querySelectorAll(".chat-user")).filter(n=>n.innerText.includes("모바일 채팅 최하단 즉시 이동 테스트"));return {count:users.length,gap:feed.scrollHeight-feed.scrollTop-feed.clientHeight,reply:feed.lastElementChild?.innerText||""};');
  assert.equal(settledSnap.count,1,'real transcript user item replaces optimistic card without duplication');
  assert(settledSnap.gap<=2,'feed stays pinned at bottom when agent reply arrives');
  assert(settledSnap.reply.includes('바로 확인했습니다.'));
  await browser('screenshot',path.join(evidence,'chat-submit-bottom-390.png'));
  // Floating bulk work-process toggle button above jump-to-bottom on 390x844.
  await evaluate(`
   view.unmount();state.chatCache.clear();
   state.sessions.set(7,{id:7,state:"Working"});
   const workItems=[
    {id:"wu1",kind:"user",text:"작업 단계 확인해줘"},
    {id:"wr1",kind:"reasoning",text:"첫 번째 작업 과정을 검토합니다."},
    {id:"wt1",kind:"tool_call",call_id:"wc1",tool_name:"read_file",file_path:"/work/app.rs"},
    {id:"wtr1",kind:"tool_result",call_id:"wc1",text:"ok"},
    {id:"wa1",kind:"assistant",text:"첫 단계 완료했습니다. ".repeat(18)},
    {id:"wr2",kind:"reasoning",text:"두 번째 작업 중 생각 과정입니다."},
    ...Array.from({length:12},(_,i)=>({id:"wt2_"+i,kind:"tool_call",call_id:"wc2_"+i,tool_name:"run_command",title:"Step "+(i+1)+" command check",text:"cmd "+i}))
   ];
   ctx.rpc=async q=>q.action==="chat_poll"?{generation:"g-work",agent:"agy",bound:true,page_start:0,next_offset:workItems.length,items:workItems}:{};
   window.view=KrillMobile.chat.mount(document.querySelector("main"),{ctx,sessionId:7});
   window.feed=document.querySelector(".chat-feed");
   return true;
  `);
  await browser('wait','--fn','document.querySelectorAll(".chat-tool-run").length===2&&!document.querySelector(".chat-process-toggle").hidden');
  const toggleOpenState=await evaluate(`
   const btn=document.querySelector(".chat-process-toggle"),jump=document.querySelector(".chat-jump-bottom"),comp=document.querySelector(".chat-composer"),runs=[...document.querySelectorAll(".chat-tool-run")];
   runs[1].open=true;runs[1].dispatchEvent(new Event("toggle"));
   await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));
   feed.scrollTop=0;feed.dispatchEvent(new Event("scroll"));
   const br=btn.getBoundingClientRect(),jr=jump.getBoundingClientRect(),cr=comp.getBoundingClientRect();
   return {btnHidden:btn.hidden,jumpHidden:jump.hidden,expanded:btn.dataset.expanded,runsOpen:runs.map(r=>r.open),br:{top:br.top,bottom:br.bottom,right:br.right,width:br.width,height:br.height},jr:{top:jr.top,bottom:jr.bottom,right:jr.right},cr:{top:cr.top}};
  `);
  assert.equal(toggleOpenState.btnHidden,false,'work-process toggle shows when work runs exist');
  assert.equal(toggleOpenState.jumpHidden,false,'jump button shows when scrolled up');
  assert.equal(toggleOpenState.expanded,'true','opening a run switches toggle to collapse-all');
  assert(Math.abs(toggleOpenState.br.right-toggleOpenState.jr.right)<=1,'toggle aligns horizontally with jump button');
  assert(toggleOpenState.br.bottom<=toggleOpenState.jr.top-6,'toggle sits cleanly above jump button');
  assert(toggleOpenState.jr.bottom<=toggleOpenState.cr.top,'floating buttons stay above composer');
  await browser('click','.chat-process-toggle');
  await browser('wait','--fn','document.querySelector(".chat-process-toggle")?.dataset?.expanded==="false"');
  const toggleCollapsed=await evaluate(`
   const btn=document.querySelector(".chat-process-toggle"),runs=[...document.querySelectorAll(".chat-tool-run")],br=btn.getBoundingClientRect();
   return {expanded:btn.dataset.expanded,runsOpen:runs.map(r=>r.open),top:br.top,bottom:br.bottom};
  `);
  assert.deepEqual(toggleCollapsed.runsOpen,[false,false],'bulk click collapses all work runs');
  assert(Math.abs(toggleCollapsed.top-toggleOpenState.br.top)<=1,'toggle stays in fixed vertical slot without jumping');
  await browser('click','.chat-process-toggle');
  await browser('wait','--fn','document.querySelector(".chat-process-toggle")?.dataset?.expanded==="true"');
  const toggleReopened=await evaluate('return [...document.querySelectorAll(".chat-tool-run")].map(r=>r.open);');
  assert.deepEqual(toggleReopened,[true,true],'second click expands all work runs');
  console.log('PASS Chromium 390x844 chat prefetch, automatic pagination, stable reader anchor, trusted touch, dictation stability, immediate scroll-to-bottom on submit and bulk work-process toggle');
 }finally{for(const p of pending.values()){clearTimeout(p.timer);p.reject(Error('CDP closed'));}socket?.close();await browser('close').catch(()=>{});await new Promise(resolve=>server.close(resolve));}
}
main().catch(error=>{console.error(error);process.exitCode=1;});
