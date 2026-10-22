// Orca mobile conversation/tool disclosure reference: MIT, Copyright (c) 2026 Lovecast Inc.
(()=>{const M=globalThis.KrillMobile||={};M.screens||={};
function renderRichText(el,text){
 const wrap=el('div',undefined,'chat-md');
 const blocks=String(text||'').split(/```/);
 for(let i=0;i<blocks.length;i++){
  const part=blocks[i];
  if(i%2===1){
   const nl=part.indexOf('\n');
   const codeText=nl>=0?part.slice(nl+1):part;
   const pre=el('pre',undefined,'chat-code'),code=el('code',codeText.replace(/\n$/,''));
   pre.append(code);wrap.append(pre);
  }else{
   for(const rawLine of part.split('\n')){
    const line=rawLine.trimEnd();
    if(!line.trim())continue;
    if(line.startsWith('### '))wrap.append(el('h4',line.slice(4),'chat-h'));
    else if(line.startsWith('## '))wrap.append(el('h3',line.slice(3),'chat-h'));
    else if(line.startsWith('# '))wrap.append(el('h2',line.slice(2),'chat-h'));
    else if(/^[-*]\s+/.test(line.trim()))wrap.append(el('p','• '+line.trim().slice(2),'chat-li'));
    else wrap.append(el('p',line,'chat-p'));
   }
  }
 }
 return wrap;
}
// Plain-language step names let non-developers skim what the agent did without opening tool payloads.
function toolStep(name){
 const n=String(name||'');
 return /^(read|view|notebookread|read_file)$/i.test(n)?'Read {0} file(s)':/edit|write|patch/i.test(n)?'Changed {0} file(s)':/grep|glob|search|find|^ls$/i.test(n)?'Searched {0} time(s)':/bash|shell|exec|command/i.test(n)?'Ran {0} command(s)':'Did {0} other step(s)';
}
function mountChat(root,opts){
 const {ctx,sessionId,onSwitchTerm}=opts;
 const {el,button,state}=ctx,drafts=state.drafts||(state.drafts=new Map()),t=ctx.t||((s)=>s),itemLimit=1000;
 const cacheMap=state.chatCache||(state.chatCache=new Map());
 const entry=cacheMap.get(sessionId)||{offset:0,items:[],agent:'',bound:false};
 entry.pages||=[];entry.photoVersion||=0;
 cacheMap.set(sessionId,entry);
 const shell=el('div',undefined,'chat-view'),feed=el('div',undefined,'chat-feed'),strip=el('div',undefined,'chat-live-strip'),composer=el('form',undefined,'chat-composer');
 const stripBadge=el('span','IDLE','badge'),stripText=el('span',t('Waiting for transcript…'),'chat-live-text'),rawBtn=button(t('Term →'),()=>onSwitchTerm?.(),'ghost small chat-raw-btn');
 strip.append(stripBadge,stripText,rawBtn);
 const history=el('div',undefined,'chat-actions'),historyText=el('span',undefined,'muted');
 const earlierBtn=button(t('Load earlier messages'),()=>run(loadEarlier),'ghost small');
 const latestBtn=button(t('Back to latest'),()=>{
  if(entry.live){entry.items=entry.live.items;entry.offset=entry.live.offset;entry.live=null;}
  renderItems();pollNow();
 },'ghost small');
 history.append(historyText,earlierBtn,latestBtn);
 const pollStatus=el('div',undefined,'chat-actions'),pollText=el('span',undefined,'muted');
 pollStatus.setAttribute('role','status');
 const retryBtn=button(t('Retry'),()=>pollNow(),'ghost small');pollStatus.append(pollText,retryBtn);pollStatus.hidden=true;
 const input=el('textarea',undefined,'chat-input');input.rows=2;input.placeholder=t('Message agent (Bracketed Paste)…');input.setAttribute('aria-label',t('Chat prompt'));
 input.value=drafts.get(sessionId)||'';
 let composing=false,disposed=false,pollTimer=null,inFlight=false,failures=0,lastSuccess=entry.lastSuccess||0,earlierPending=false;
 const cards=new Map(),toolRuns=new Map();
 input.addEventListener('input',()=>{entry.draftVersion=(entry.draftVersion||0)+1;drafts.set(sessionId,input.value);});
 input.addEventListener('compositionstart',()=>composing=true);
 input.addEventListener('compositionend',()=>composing=false);
 const attachment=el('div',undefined,'chat-actions');attachment.setAttribute('role','status');
 const attachBtn=button(t('Photo'),()=>{
  if(entry.photo||entry.sending)return;
  const picker=el('input');picker.type='file';picker.accept='image/*';
  picker.addEventListener('change',()=>{const f=picker.files?.[0];if(f)preparePhoto(f);});if(M.updates?.pickFile)M.updates.pickFile(picker);else picker.click?.();
 },'ghost small');
 const sendBtn=el('button',t('Send'),'primary small');sendBtn.type='submit';
 const bar=el('div',undefined,'chat-composer-bar');bar.append(attachBtn,sendBtn);
 composer.append(input,attachment,bar);
 // act owns product-level error presentation; local operations retain their own visible status.
 function run(fn){if(ctx.act)ctx.act(fn);else Promise.resolve().then(fn).catch(e=>ctx.error?.(e));}
 function updateComposer(sent){
  // Completion belongs to the shared draft version, including a newly mounted composer.
  if(sent&&(entry.draftVersion||0)===sent.version&&input.value===sent.text)input.value='';
  sendBtn.disabled=entry.sending||entry.photo?.status==='pending'||entry.photo?.status==='error';
  attachBtn.disabled=entry.sending||!!entry.photo;
  attachment.replaceChildren();
  const photo=entry.photo;if(!photo)return;
  const text=photo.status==='pending'?t('Preparing photo…'):photo.status==='error'?t('Photo preparation failed. Retry or remove it.'):t('Photo ready. It will be sent with this message.');
  attachment.append(el('span',`${photo.name}: ${text}`,photo.status==='error'?'danger':'muted'));
  if(photo.status==='error')attachment.append(button(t('Retry photo'),()=>preparePhoto(photo.file),'ghost small'));
  attachment.append(button(t('Remove photo'),()=>{if(entry.sending)return;entry.photoVersion++;entry.photo=null;updateComposer();},'ghost small'));
 }
 async function preparePhoto(file){
  const version=++entry.photoVersion;
  entry.photo={file,name:file.name||t('Photo'),status:'pending',version};updateComposer();
  try{
   const prepared=await ctx.photo?.(sessionId,file,{prepare:true});
   if(entry.photoVersion!==version)return;
   if(!prepared?.mime||!prepared?.data)throw new Error(t('Photo preparation unavailable'));
   entry.photo={...entry.photo,status:'ready',prepared};
  }catch(error){if(entry.photoVersion===version)entry.photo={...entry.photo,status:'error',error:String(error?.message||error)};}
  entry.notifyComposer?.();
 }
 composer.addEventListener('submit',e=>{
  e.preventDefault();if(composing||entry.sending||entry.photo?.status==='pending'||entry.photo?.status==='error')return;
  const text=input.value,photo=entry.photo,version=entry.draftVersion||0;
  if(!text.trim()&&!photo)return;
  entry.sending=true;updateComposer();
  run(async()=>{
   try{
    await ctx.rpc({action:'prompt',session:sessionId,text,...(photo?{photo:photo.prepared}:{})});
    // A later mount or edit owns the current draft; an older RPC must never erase it.
    if((entry.draftVersion||0)===version&&drafts.get(sessionId)===text){
     drafts.delete(sessionId);entry.notifyComposer?.({version,text});
    }
    if(photo&&entry.photo===photo)entry.photo=null;
    if(!disposed){await pollNow();await ctx.refresh?.();}
   }finally{entry.sending=false;entry.notifyComposer?.();}
  });
 });
 shell.append(history,pollStatus,feed,strip,composer);root.replaceChildren(shell);
 function itemKey(item,idx){return `${entry.generation||''}:${item.id||`${item._pageStart||0}:${idx}:${item.tool_id||''}:${item.text}`}`;}
 function renderItems(){
  const stick=!feed.scrollHeight||(feed.scrollHeight-(feed.scrollTop||0)-(feed.clientHeight||0)<60);
  const wanted=new Set(),wantedRuns=new Set(),nodes=[];let run=null;
  // Retain the actual question/details nodes so selection, write-in, focus and expansion survive deltas.
  for(const [idx,item] of entry.items.entries()){
   const key=itemKey(item,idx);wanted.add(key);
   let card=cards.get(key);
   if(!card){card=renderCard(item,key);card.dataset.chatId=key;cards.set(key,card);}
   if(item.kind==='tool_call'||item.kind==='tool_result'){
    if(!run){let group=toolRuns.get(key);if(!group){const details=el('details',undefined,'chat-tool-run'),summary=el('summary',undefined,'chat-tool-summary'),body=el('div',undefined,'chat-tool-items');rememberDetails(details,'run:'+key);details.append(summary,body);group={details,summary,body};toolRuns.set(key,group);}run={...group,key,nodes:[],steps:new Map(),count:0,errors:0};wantedRuns.add(key);nodes.push(run.details);}
    run.nodes.push(card);if(item.is_error)run.errors++;if(item.kind==='tool_call'){run.count++;const step=toolStep(item.tool_name);run.steps.set(step,(run.steps.get(step)||0)+1);}run.summary.textContent=[run.errors||!run.count?t(run.errors?'Tool error':'Tool activity'):'',...[...run.steps].map(([step,n])=>t(step,n))].filter(Boolean).join(' · ');
    const at=run.body.children?.[run.nodes.length-1];if(at!==card)run.body.insertBefore(card,at||null);
   }else{run=null;nodes.push(card);}
  }
  for(const [idx,node] of nodes.entries()){const at=feed.children?.[idx];if(at!==node)feed.insertBefore(node,at||null);}
  for(const [key,group] of toolRuns)if(!wantedRuns.has(key)){group.details.remove();toolRuns.delete(key);}
  for(const [key,card] of cards)if(!wanted.has(key)){card.remove();cards.delete(key);}
  for(const child of Array.from(feed.children||[]))if(child.className==='chat-empty')child.remove();
  if(!entry.items.length){
   const empty=el('div',undefined,'chat-empty');
   empty.append(el('p',t(entry.bound?'No chat turns recorded yet.':'Waiting for agent hook or transcript binding…'),'muted'));
   feed.append(empty);
  }
  const first=entry.items[0];
  const start=first?._pageStart||0;
  earlierBtn.hidden=!entry.pages.some(page=>page.start<start)&&!first?._clipped;
  earlierBtn.disabled=earlierPending;
  latestBtn.hidden=!entry.live;
  historyText.textContent=t(entry.live?'Reading earlier messages; live updates are paused.':'Showing up to 1000 messages. Load earlier messages to read more.');
  history.hidden=earlierBtn.hidden&&latestBtn.hidden;
  if(stick&&!entry.live)feed.scrollTop=feed.scrollHeight||0;
 }
 function agentName(){const a=entry.agent||t('Agent');return a[0].toUpperCase()+a.slice(1);}
 function rememberDetails(node,key){node.open=entry.disclosures?.get(key)||false;node.addEventListener('toggle',()=>{entry.disclosures||=new Map();entry.disclosures.set(key,node.open);});}
 function renderCard(item,key){
  const kind=item.kind||'assistant',card=el('article',undefined,`chat-card chat-${kind}`);
  if(kind==='user'||kind==='assistant')card.append(el('header',kind==='user'?t('My request'):t('{0} reply',agentName()),'chat-role'),renderRichText(el,item.text));
  else if(kind==='reasoning'||kind==='tool_result'){
   const details=el('details',undefined,kind==='reasoning'?'chat-reasoning':'chat-tool-result');
   rememberDetails(details,key);details.append(el('summary',t(kind==='reasoning'?'Reasoning':item.is_error?'Tool error':'Tool output')),kind==='reasoning'?renderRichText(el,item.text):el('pre',item.text,'chat-code'));card.append(details);
  }else if(kind==='tool_call'){
   const details=el('details',undefined,'chat-tool-call'),head=el('summary',undefined,'chat-tool-head');head.append(el('span',item.tool_name||'tool','badge'),el('span',item.title||item.file_path||t('Tool call'),'chat-tool-title'));rememberDetails(details,key);details.append(head);
   if(item.file_path)details.append(button(t('Current diff'),()=>openDiff(item.file_path),'ghost small'));
   if(item.text&&item.text!==item.file_path)details.append(el('pre',item.text,'chat-tool-body'));card.append(details);
  }else if(kind==='question')renderQuestionCard(card,item);
  else if(kind==='permission'){
   card.append(el('header',`${t('Permission')} • ${item.title||item.tool_name||t('Approval')}`,'chat-role'),el('p',item.text,'chat-p'),el('p',t('Historical request. Confirm the current request in Term.'),'muted'),button(t('Confirm in Term'),()=>onSwitchTerm?.(),'ghost small'));
  }
  return card;
 }
 function renderQuestionCard(card,item){
  card.append(el('header',item.title?`${t('Question')} • ${item.title}`:t('Question'),'chat-role'),el('p',item.text,'chat-p'));
  const list=el('div',undefined,'chat-question-options'),selected=new Set(),buttons=[];
  for(const [index,opt] of (item.options||[]).entries()){
   const b=button(opt.description?`${opt.label} — ${opt.description}`:opt.label,()=>{
    if(!item.multi_select)selected.clear();
    if(selected.has(index))selected.delete(index);else selected.add(index);
    buttons.forEach((b,i)=>{b.className=`${selected.has(i)?'primary':'ghost'} small chat-opt`;b.setAttribute('aria-pressed',String(selected.has(i)));});
   },'ghost small chat-opt');b.setAttribute('aria-pressed','false');buttons.push(b);list.append(b);
  }
  card.append(list);
  const custom=el('input');custom.type='text';custom.placeholder=t('Other (write-in)…');custom.setAttribute('aria-label',t('Other answer notes'));
  card.append(custom,el('p',t('These are answer notes only. Confirm the current question and enter your answer in Term.'),'muted'),button(t('Confirm in Term'),()=>onSwitchTerm?.(),'ghost small'));
 }
 function openDiff(filePath){
  run(async()=>{
   const sess=state.sessions?.get(sessionId),cwd=(sess?.cwd||'').replace(/\/+$/,'');
   const relPath=cwd&&filePath.startsWith(cwd+'/')?filePath.slice(cwd.length+1):filePath;
   const data=await ctx.rpc({action:'git',session:sessionId,path:relPath});
   ctx.sheet?.(t('Current diff')+`: ${filePath}`,[],undefined,body=>{
    const message=data?.diff_status==='outside_scope'?t('This file is outside the current workspace diff.'):t('No current changes for this file. This is a historical tool record; its earlier diff is not stored here.');
    body.append(el('pre',data?.patch||message,data?.patch?'chat-code':'muted'));
   });
  });
 }
 function updateStrip(){
  const sess=state.sessions?.get(sessionId);stripBadge.textContent=sess?.state||'Idle';
  stripText.textContent=sess?.lines?.at(-1)||(entry.bound?`${entry.agent||'agent'} ${t('bound')}`:t('PTY live'));
 }
 function normalizeItems(res,start){return (res.items||[]).map((item,index)=>({...item,id:item.id||`${start}:${index}`,_pageStart:start}));}
 function clearTranscript(){entry.items=[];entry.pages=[];entry.live=null;entry.agent='';entry.bound=false;cards.clear();toolRuns.clear();feed.replaceChildren();}
 function rememberPage(start,next){if(next>start&&!entry.pages.some(p=>p.start===start)){entry.pages.push({start,next});entry.pages.sort((a,b)=>a.start-b.start);}}
 function trimItems(fromStart){
  if(entry.items.length<=itemLimit)return;
  if(fromStart){
   const removed=entry.items.splice(0,entry.items.length-itemLimit),first=entry.items[0];
   // Re-read this page only when it actually contained an item that was clipped.
   if(first&&removed.at(-1)?._pageStart===first._pageStart)first._clipped=true;
  }
  else entry.items.length=itemLimit;
 }
 function showPollError(error){
  failures++;pollStatus.hidden=false;
  const when=lastSuccess?new Date(lastSuccess).toLocaleTimeString():t('never');
  pollText.textContent=`${t('Chat history update failed')} (${failures}). ${t('Last updated')}: ${when}. ${t('Retry or check Term.')} ${String(error?.message||error||'')}`;
 }
 async function pollNow(){
  if(disposed||inFlight||earlierPending||entry.live||state.connection?.phase!=='ready')return;
  inFlight=true;
  try{
   const start=entry.offset,res=await ctx.rpc({action:'chat_poll',session:sessionId,offset:start});
   if(disposed||earlierPending||entry.live||!res)return;
   const changed=entry.generation&&res.generation&&entry.generation!==res.generation;
   if(res.reset||changed){clearTranscript();entry.offset=0;}
   entry.generation=res.generation||entry.generation;entry.agent=res.agent||'';entry.bound=!!res.bound;
   // A newly bound transcript must be read from its beginning, not from the old transcript offset.
   if(changed&&!res.reset&&start!==0){renderItems();return;}
   const effectiveStart=res.reset?0:start;
   const incoming=normalizeItems(res,effectiveStart),seen=new Set(entry.items.map(item=>item.id));
   entry.items.push(...incoming.filter(item=>!seen.has(item.id)));
   if(incoming.length)rememberPage(effectiveStart,res.next_offset);trimItems(true);
   if(typeof res.next_offset==='number')entry.offset=res.next_offset;
   lastSuccess=entry.lastSuccess=Date.now();failures=0;pollStatus.hidden=true;
   renderItems();updateStrip();
  }catch(error){if(!disposed)showPollError(error);}finally{inFlight=false;}
 }
 async function loadEarlier(){
  if(disposed||earlierPending||inFlight||!entry.items.length)return;
  earlierPending=true;renderItems();
  const anchor=Array.from(feed.children||[]).find(card=>(card.offsetTop||0)>=(feed.scrollTop||0))||feed.children?.[0];
  const oldTop=anchor?.offsetTop||0,oldScroll=feed.scrollTop||0,first=entry.items[0];
  const page=first._clipped?entry.pages.find(page=>page.start===first._pageStart):entry.pages.filter(page=>page.start<first._pageStart).at(-1);
  const start=page?.start;
  if(start===undefined){earlierPending=false;renderItems();return;}
  try{
   const res=await ctx.rpc({action:'chat_poll',session:sessionId,offset:start,end_offset:page.next});
   if(disposed)return;
   if(res.next_offset!==page.next)throw new Error(t('Transcript changed. Return to latest and retry.'));
   if(res.reset||(res.generation&&entry.generation&&res.generation!==entry.generation))throw new Error(t('Transcript changed. Return to latest and retry.'));
   const items=normalizeItems(res,start),index=items.findIndex(item=>item.id===first.id);
   const prefix=index>=0?items.slice(0,index):items;
   if(!entry.live)entry.live={items:entry.items.map(item=>({...item})),offset:entry.offset};
   const seen=new Set(entry.items.map(item=>item.id));entry.items.unshift(...prefix.filter(item=>!seen.has(item.id)));trimItems(false);
   if(first._clipped)delete first._clipped;
   pollStatus.hidden=true;renderItems();
   feed.scrollTop=oldScroll+((anchor?.offsetTop||0)-oldTop);
  }catch(error){showPollError(error);}finally{earlierPending=false;if(!disposed)renderItems();}
 }
 function schedule(){clearTimeout(pollTimer);if(!disposed)pollTimer=setTimeout(async()=>{await pollNow();schedule();},1500);}
 entry.notifyComposer=updateComposer;
 renderItems();updateStrip();updateComposer();pollNow();schedule();
 return {update(){updateStrip();updateComposer();pollNow();},unmount(){disposed=true;clearTimeout(pollTimer);pollTimer=null;if(entry.notifyComposer===updateComposer)entry.notifyComposer=null;}};
}
M.chat={mount:mountChat};
M.screens.chat=(root,ctx)=>{
 const session=ctx.state.sessions.get(ctx.route.sessionId);
 if(!session){root.append(ctx.el('p',(ctx.t||((s)=>s))('Session not found'),'empty'));return {};}
 const handle=mountChat(root,{ctx,sessionId:session.id,onSwitchTerm:()=>ctx.go(`/session/${session.id}`)});
 return {update:()=>handle.update(),cleanup:()=>handle.unmount()};
};
})();
