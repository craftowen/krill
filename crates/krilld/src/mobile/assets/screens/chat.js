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
function mountChat(root,opts){
 const {ctx,sessionId,onSwitchTerm}=opts;
 const {el,button,state}=ctx;
 const cacheMap=state.chatCache||(state.chatCache=new Map());
 const entry=cacheMap.get(sessionId)||{offset:0,items:[],agent:'',bound:false,answered:new Set()};
 cacheMap.set(sessionId,entry);
 const shell=el('div',undefined,'chat-view'),feed=el('div',undefined,'chat-feed'),strip=el('div',undefined,'chat-live-strip'),composer=el('form',undefined,'chat-composer');
 const stripBadge=el('span','IDLE','badge'),stripText=el('span','Waiting for transcript…','chat-live-text'),rawBtn=button('Term →',()=>onSwitchTerm?.(),'ghost small chat-raw-btn');
 strip.append(stripBadge,stripText,rawBtn);
 const input=el('textarea',undefined,'chat-input');input.rows=2;input.placeholder='Message agent (Bracketed Paste)…';input.setAttribute('aria-label','Chat prompt');
 input.value=state.drafts?.get(sessionId)||'';
 let composing=false,disposed=false,pollTimer=null,inFlight=false;
 input.addEventListener('input',()=>state.drafts?.set(sessionId,input.value));
 input.addEventListener('compositionstart',()=>composing=true);
 input.addEventListener('compositionend',()=>composing=false);
 const attachBtn=button('Photo',()=>{const picker=el('input');picker.type='file';picker.accept='image/*';picker.addEventListener('change',()=>{const f=picker.files?.[0];if(f)ctx.photo?.(sessionId,f);});picker.click?.();},'ghost small');
 const sendBtn=el('button','Send','primary small');sendBtn.type='submit';
 const bar=el('div',undefined,'chat-composer-bar');bar.append(attachBtn,sendBtn);
 composer.append(input,bar);
 composer.addEventListener('submit',e=>{
  e.preventDefault();if(composing)return;
  const text=input.value;if(!text.trim())return;
  ctx.act(async()=>{
   sendBtn.disabled=true;
   try{
    await ctx.rpc({action:'prompt',session:sessionId,text});
    input.value='';state.drafts?.delete(sessionId);
    await pollNow();await ctx.refresh?.();
   }finally{sendBtn.disabled=false;}
  });
 });
 shell.append(feed,strip,composer);root.replaceChildren(shell);

 function renderItems(){
  feed.replaceChildren();
  const sess=state.sessions?.get(sessionId);
  if(!entry.items.length){
   const empty=el('div',undefined,'chat-empty');
   empty.append(el('p',entry.bound?'No chat turns recorded yet.':'Waiting for agent hook or transcript binding…','muted'));
   if(sess?.lines?.length){
    const pre=el('pre',sess.lines.join('\n'),'chat-code');
    empty.append(pre);
   }
   feed.append(empty);
   return;
  }
  entry.items.forEach((item,idx)=>{
   const kind=item.kind||'assistant';
   const card=el('article',undefined,`chat-card chat-${kind}`);
   if(kind==='user'){
    card.append(el('header','You','chat-role'),renderRichText(el,item.text));
   }else if(kind==='assistant'){
    card.append(el('header',entry.agent||'Agent','chat-role'),renderRichText(el,item.text));
   }else if(kind==='reasoning'){
    const details=el('details',undefined,'chat-reasoning'),sum=el('summary','Reasoning');
    details.append(sum,renderRichText(el,item.text));card.append(details);
   }else if(kind==='tool_call'){
    const head=el('div',undefined,'chat-tool-head');
    head.append(el('span',item.tool_name||'tool','badge'),el('span',item.title||item.file_path||item.text||'Tool call','chat-tool-title'));
    if(item.file_path){
     const diffBtn=button('Diff',()=>openDiff(item.file_path),'ghost small');
     head.append(diffBtn);
    }
    card.append(head);
    if(item.text&&item.text!==item.file_path)card.append(el('pre',item.text,'chat-tool-body'));
   }else if(kind==='tool_result'){
    const details=el('details',undefined,'chat-tool-result'),sum=el('summary',item.is_error?'Tool error':'Tool output');
    details.append(sum,el('pre',item.text,'chat-code'));card.append(details);
   }else if(kind==='question'){
    renderQuestionCard(card,item,idx);
   }else if(kind==='permission'){
    const head=el('header',`Permission • ${item.title||item.tool_name||'Approval'}`,'chat-role');
    const body=el('p',item.text,'chat-p');
    const row=el('div',undefined,'chat-actions');
    row.append(
     button('Approve (y)',()=>ctx.act(async()=>{await ctx.rpc({action:'approve',session:sessionId,value:'y'});await ctx.refresh?.();}),'primary small'),
     button('Deny (n)',()=>ctx.act(async()=>{await ctx.rpc({action:'approve',session:sessionId,value:'n'});await ctx.refresh?.();}),'danger small')
    );
    card.append(head,body,row);
   }
   feed.append(card);
  });
  feed.scrollIntoView?.({block:'end'});
 }

 function renderQuestionCard(card,item,idx){
  const key=`${item.tool_id||idx}:${item.text}`;
  const done=entry.answered.has(key);
  card.append(el('header',item.title?`Question • ${item.title}`:'Question','chat-role'),el('p',item.text,'chat-p'));
  const opts=Array.isArray(item.options)?item.options:[];
  const selected=new Set();
  const list=el('div',undefined,'chat-question-options');
  opts.forEach((opt,optIdx)=>{
   const label=opt.description?`${opt.label} — ${opt.description}`:opt.label;
   const btn=button(label,()=>{
    if(done)return;
    if(item.multi_select){
     if(selected.has(optIdx)){selected.delete(optIdx);btn.className='ghost small chat-opt';}
     else{selected.add(optIdx);btn.className='primary small chat-opt';}
    }else{
     submitAnswer(key,[optIdx],opts.length,false,'');
    }
   },'ghost small chat-opt');
   btn.disabled=done;list.append(btn);
  });
  card.append(list);
  const customWrap=el('div',undefined,'chat-question-custom'),customInput=el('input');
  customInput.type='text';customInput.placeholder='Other (write-in)…';customInput.disabled=done;
  const submitBtn=button(done?'Answered':'Submit',()=>{
   if(done)return;
   submitAnswer(key,[...selected],opts.length,!!item.multi_select,customInput.value);
  },'primary small');
  submitBtn.disabled=done;
  customWrap.append(customInput,submitBtn);card.append(customWrap);
 }

 function submitAnswer(key,selected,optionCount,multiSelect,customText){
  ctx.act(async()=>{
   await ctx.rpc({
    action:'answer_question',
    session:sessionId,
    selected,
    option_count:optionCount,
    multi_select:multiSelect,
    custom_text:customText&&customText.trim()?customText.trim():null
   });
   entry.answered.add(key);renderItems();await ctx.refresh?.();
  });
 }

 function openDiff(filePath){
  ctx.act(async()=>{
   const data=await ctx.rpc({action:'git',session:sessionId,path:filePath});
   ctx.sheet?.(filePath,[],undefined,body=>{
    const pre=el('pre',data?.patch||'No unstaged/HEAD diff for this file.','chat-code');
    body.append(pre);
   });
  });
 }

 function updateStrip(){
  const sess=state.sessions?.get(sessionId);
  const st=sess?.state||'Idle';
  stripBadge.textContent=st;
  const lastLine=sess?.lines?.length?sess.lines[sess.lines.length-1]:(entry.bound?`${entry.agent||'agent'} bound`:'PTY live');
  stripText.textContent=lastLine;
 }

 async function pollNow(){
  if(disposed||inFlight||state.connection?.phase!=='ready')return;
  inFlight=true;
  try{
   const res=await ctx.rpc({action:'chat_poll',session:sessionId,offset:entry.offset});
   if(disposed||!res)return;
   entry.agent=res.agent||entry.agent;
   entry.bound=!!res.bound;
   if(res.reset){entry.items=[];}
   if(Array.isArray(res.items)&&res.items.length){
    entry.items.push(...res.items);
    if(entry.items.length>250)entry.items.splice(0,entry.items.length-250);
    renderItems();
   }else if(entry.offset===0){
    renderItems();
   }
   if(typeof res.next_offset==='number')entry.offset=res.next_offset;
   updateStrip();
  }catch(_){}finally{
   inFlight=false;
  }
 }

 function schedule(){
  clearTimeout(pollTimer);
  if(disposed)return;
  pollTimer=setTimeout(async()=>{await pollNow();schedule();},1500);
 }

 renderItems();updateStrip();pollNow();schedule();
 return {
  update(){updateStrip();pollNow();},
  unmount(){disposed=true;clearTimeout(pollTimer);pollTimer=null;}
 };
}
M.chat={mount:mountChat};
M.screens.chat=(root,ctx)=>{
 const session=ctx.state.sessions.get(ctx.route.sessionId);
 if(!session){root.append(ctx.el('p','Session not found','empty'));return {};}
 const handle=mountChat(root,{ctx,sessionId:session.id,onSwitchTerm:()=>ctx.go(`/session/${session.id}`)});
 return {update:()=>handle.update(),cleanup:()=>handle.unmount()};
};
})();
