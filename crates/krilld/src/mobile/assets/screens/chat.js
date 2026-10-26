// Orca mobile conversation/tool disclosure reference: MIT, Copyright (c) 2026 Lovecast Inc.
(()=>{const M=globalThis.KrillMobile||={};M.screens||={};
// Build inline formatting with text nodes/elements only; transcript HTML never becomes markup.
function renderInline(el,parent,text,depth=0){
 if(depth>=8){parent.append(el('span',text));return;}
 const tokens=/\\([\\`*_[\]])|(`+)(.*?)\2(?!`)|(\*\*|__|\*)(?=\S)(.+?\S|\S)\4|\[([^\]\n]+)\]\(([^)\s]+)\)/g;
 let end=0;
 for(const match of text.matchAll(tokens)){
  if(match.index>end)parent.append(el('span',text.slice(end,match.index)));
  const [,escaped,ticks,code,mark,body,label,href]=match;
  if(escaped)parent.append(el('span',escaped));
  else if(ticks)parent.append(el('code',code,'chat-inline-code'));
  else if(mark){const node=el(mark.length===2?'strong':'em');renderInline(el,node,body,depth+1);parent.append(node);}
  else if(/^(https?:\/\/|mailto:)/i.test(href)){
   const node=el('a');node.setAttribute('href',href);node.setAttribute('target','_blank');node.setAttribute('rel','noopener noreferrer');renderInline(el,node,label,depth+1);parent.append(node);
  }else parent.append(el('span',match[0]));
  end=match.index+match[0].length;
 }
 if(end<text.length)parent.append(el('span',text.slice(end)));
}
function renderRichText(el,text){
 const wrap=el('div',undefined,'chat-md'),blocks=String(text||'').split(/```/);
 const rich=(tag,value,cls)=>{const node=el(tag,undefined,cls);renderInline(el,node,value);return node;};
 const cells=line=>line.trim().replace(/^\|/,'').replace(/\|$/,'').split('|').map(s=>s.trim());
 const tableLine=line=>/^\s*\|.*\|\s*$/.test(line),separator=line=>cells(line).every(s=>/^:?-{3,}:?$/.test(s));
 for(let i=0;i<blocks.length;i++){
  const part=blocks[i];
  if(i%2===1){
   const nl=part.indexOf('\n'),codeText=nl>=0?part.slice(nl+1):part,pre=el('pre',undefined,'chat-code');
   pre.append(el('code',codeText.replace(/\n$/,'')));wrap.append(pre);continue;
  }
  const lines=part.split('\n');let list=null,quote=null;
  for(let n=0;n<lines.length;n++){
   const line=lines[n].trimEnd(),trimmed=line.trim();
   if(!trimmed){list=quote=null;continue;}
   if(tableLine(line)){
    list=quote=null;const table=el('table',undefined,'chat-table'),scroll=el('div',undefined,'chat-code'),header=tableLine(lines[n+1]||'')&&separator(lines[n+1]);
    let first=true;
    while(n<lines.length&&tableLine(lines[n])){const row=el('tr');if(!separator(lines[n])){for(const value of cells(lines[n]))row.append(rich(first&&header?'th':'td',value));table.append(row);first=false;}n++;}
    n--;scroll.append(table);wrap.append(scroll);continue;
   }
   const heading=trimmed.match(/^(#{1,3})\s+(.*)$/),bullet=trimmed.match(/^(?:([-*])|([0-9]+)[.)])\s+(.*)$/),quoted=trimmed.match(/^>\s?(.*)$/);
   if(quoted){list=null;if(!quote){quote=el('blockquote',undefined,'chat-quote');wrap.append(quote);}quote.append(rich('p',quoted[1],'chat-p'));}
   else if(bullet){quote=null;const tag=bullet[2]?'ol':'ul';if(!list||list.dataset.listKind!==tag){list=el(tag,undefined,tag==='ol'?'chat-ol':undefined);list.dataset.listKind=tag;if(bullet[2])list.setAttribute('start',bullet[2]);wrap.append(list);}list.append(rich('li',bullet[3],'chat-li'));}
   else{list=quote=null;wrap.append(rich(heading?'h'+(heading[1].length+1):'p',heading?heading[2]:line,heading?'chat-h':'chat-p'));}
  }
 }
 return wrap;
}
// Plain-language step names let non-developers skim what the agent did without opening tool payloads.
function toolStep(name){
 const n=String(name||'');
 return /^(read|view|notebookread|read_file)$/i.test(n)?'Read {0} file(s)':/edit|write|patch/i.test(n)?'Changed {0} file(s)':/grep|glob|search|find|^ls$/i.test(n)?'Searched {0} time(s)':/bash|shell|exec|command/i.test(n)?'Ran {0} command(s)':'Did {0} other step(s)';
}
const cacheLimit=8,cacheByteLimit=8*1024*1024,pageLimit=2048,itemByteLimit=2*1024*1024;
const pinned=(state,id,e)=>e.mounts>0||e.sending||e.photo||e.live||!!state.drafts?.get(id);
function pruneCache(state){const cache=state.chatCache;if(!cache)return;let bytes=0;for(const e of cache.values())bytes+=e.retainedBytes||0;for(const [id,e] of cache){if(cache.size<=cacheLimit&&bytes<=cacheByteLimit)break;if(!pinned(state,id,e)){cache.delete(id);bytes-=e.retainedBytes||0;}}}
function forgetSession(state,id){const e=state.chatCache?.get(id);if(e&&!pinned(state,id,e))state.chatCache.delete(id);}
function mountChat(root,opts){
 const {ctx,sessionId,onSwitchTerm,agentIcon}=opts,agentName=a=>M.agentName?.(a)||a;
 const {el,button,state}=ctx,drafts=state.drafts||(state.drafts=new Map()),t=ctx.t||((s)=>s),itemLimit=1000;
 const cacheMap=state.chatCache||(state.chatCache=new Map());
 const entry=cacheMap.get(sessionId)||{offset:0,items:[],agent:'',bound:false};
 entry.pages||=[];entry.photoVersion||=0;
 entry.mounts=(entry.mounts||0)+1;cacheMap.delete(sessionId);cacheMap.set(sessionId,entry);pruneCache(state);
 const shell=el('div',undefined,'chat-view'),feed=el('div',undefined,'chat-feed'),strip=el('div',undefined,'chat-live-strip'),composer=el('form',undefined,'chat-composer');
 const stripBadge=el('span','IDLE','badge'),stripText=el('span',t('Waiting for transcript…'),'chat-live-text'),rawBtn=button(t('Term →'),()=>onSwitchTerm?.(),'ghost small chat-raw-btn');
 strip.append(stripBadge,stripText,rawBtn);
 const history=el('div',undefined,'chat-actions chat-history'),historyText=el('span',undefined,'muted');
 const earlierBtn=button(t('Load earlier messages'),()=>run(loadEarlier),'ghost small');
 const latestBtn=button(t('Back to latest'),()=>{
  if(entry.live){entry.items=entry.live.items;entry.offset=entry.live.offset;entry.live=null;}
  following=true;renderItems();pollNow();
 },'ghost small');
 history.append(historyText,earlierBtn,latestBtn);
 const pollStatus=el('div',undefined,'chat-actions'),pollText=el('span',undefined,'muted');
 pollStatus.setAttribute('role','status');
 const retryBtn=button(t('Retry'),()=>pollNow(),'ghost small');pollStatus.append(pollText,retryBtn);pollStatus.hidden=true;
 const input=el('textarea',undefined,'chat-input');input.rows=1;input.placeholder=t('Message agent (Bracketed Paste)…');input.setAttribute('aria-label',t('Chat prompt'));
 function sizeInput(){if(!input.style)return;input.style.height='auto';if(input.scrollHeight)input.style.height=Math.min(140,Math.max(28,input.scrollHeight))+'px';}
 input.value=drafts.get(sessionId)||'';
 function insertText(text){
  const clean=String(text??'').replace(/\r\n?/g,'\n').replace(/^\n+|\n+$/g,'');if(!clean)return false;
  const value=input.value,start=typeof input.selectionStart==='number'?input.selectionStart:value.length,end=typeof input.selectionEnd==='number'?input.selectionEnd:start;
  input.value=value.slice(0,start)+clean+value.slice(end);input.selectionStart=input.selectionEnd=start+clean.length;
  entry.draftVersion=(entry.draftVersion||0)+1;drafts.set(sessionId,input.value);sizeInput();input.focus?.();return true;
 }

 let composing=false,disposed=false,pollTimer=null,inFlight=false,retryWithoutTail=false,failures=0,lastSuccess=entry.lastSuccess||0,earlierPending=false,visible=opts.visible!==false,following=true;
 entry.initialized??=entry.offset>0;
 entry.loaded??=entry.initialized;
 let refreshLatest=!!entry.initialized&&Date.now()-lastSuccess>3000;
 // Only the reader moving up stops following; pins and late layout growth never do.
 let lastTop=0;feed.addEventListener('scroll',()=>{if(!visible)return;const top=feed.scrollTop,gap=feed.scrollHeight-top-feed.clientHeight;if(gap<=1)following=true;else if(top<lastTop-1)following=false;else if(gap<60)following=true;lastTop=top;});
 function pinLatest(){if(visible&&following&&!entry.live){feed.scrollTop=feed.scrollHeight||0;lastTop=feed.scrollTop;}}
 // Late fonts, disclosures, a shown pane or keyboard resize re-pin a following reader.
 const resize=globalThis.ResizeObserver?new ResizeObserver(()=>pinLatest()):null;resize?.observe(feed);
 const cards=new Map(),toolRuns=new Map();let tailRun=null,renderedCount=0,renderedItems=null;
 input.addEventListener('input',()=>{entry.draftVersion=(entry.draftVersion||0)+1;input.value?drafts.set(sessionId,input.value):drafts.delete(sessionId);sizeInput();});
 input.addEventListener('compositionstart',()=>composing=true);
 input.addEventListener('compositionend',()=>composing=false);
 const attachment=el('div',undefined,'chat-actions');attachment.setAttribute('role','status');
 const attachBtn=button(t('Photo'),()=>{
  if(entry.photo||entry.sending)return;
  const picker=el('input');picker.type='file';picker.accept='image/*';
  picker.addEventListener('change',()=>{const f=picker.files?.[0];if(f)preparePhoto(f);});if(M.updates?.pickFile)M.updates.pickFile(picker);else picker.click?.();
 },'icon-button composer-icon','plus');
 // Orca 컴포저: 둥근 카드 안에 입력, 아래 줄에 사진(+)·모델·전송(↑)을 둔다.
 const sendBtn=el('button',undefined,'primary composer-send');sendBtn.type='submit';sendBtn.setAttribute('aria-label',t('Send'));if(ctx.icon)sendBtn.append(ctx.icon('arrowUp'));else sendBtn.textContent=t('Send');
 const modelBtn=button(t('Model'),()=>chooseModel(),'composer-pill');
 const bar=el('div',undefined,'chat-composer-bar'),box=el('div',undefined,'chat-composer-box');bar.append(attachBtn);
 if(typeof opts.onQuickActions==='function'){const quick=button(t('Quick actions'),()=>opts.onQuickActions(),'btn chat-quick-actions-btn');quick.type='button';quick.setAttribute('aria-label',t('Quick actions'));quick.title=t('Quick actions');const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('viewBox','0 0 24 24');svg.setAttribute('aria-hidden','true');const path=document.createElementNS('http://www.w3.org/2000/svg','path');path.setAttribute('d','m5 6 6 6-6 6m8 0h6');path.setAttribute('fill','none');path.setAttribute('stroke','currentColor');path.setAttribute('stroke-width','2');svg.append(path);quick.replaceChildren(svg);bar.append(quick);}
 bar.append(modelBtn,el('span',undefined,'composer-spacer'),sendBtn);
 box.append(input,bar);composer.append(attachment,box);globalThis.MkKeyboard?.keepFocus(composer,ctx.signal);
 // 모델 전환은 에이전트 자체의 /model 명령으로 보낸다. Claude는 별칭으로 바로 바꾸고, Codex·Gemini는 터미널의 선택 화면에서 고른다.
 const modelChoices={claude:[['default','Default (recommended)'],['opus','Opus'],['sonnet','Sonnet'],['haiku','Haiku']]},modelPicker=new Set(['codex','gemini']);
 function chatAgent(){return M.sessionAgent?.({...state.sessions?.get(sessionId),...(entry.agent?{agent:entry.agent}:{})})||entry.agent||'';}
 function chooseModel(){
  const agent=chatAgent(),choices=modelChoices[agent];
  if(choices){ctx.sheet(t('Model'),choices.map(([id,label])=>({label:t(label),action:()=>run(async()=>{await ctx.rpc({action:'prompt',session:sessionId,text:`/model ${id}`});entry.model={agent,id,label};if(!disposed&&!ctx.signal?.aborted)syncAgent();})})),t('Sends /model to {0}.',agentName(agent)));return;}
  run(async()=>{await ctx.rpc({action:'prompt',session:sessionId,text:'/model'});onSwitchTerm?.();});
 }
 function syncAgent(){const agent=chatAgent(),label=entry.model?.agent===agent?t(entry.model.label):t('Model');if(modelBtn.textContent!==label){modelBtn.textContent=label;modelBtn.setAttribute('aria-label',label);}modelBtn.hidden=!modelChoices[agent]&&!modelPicker.has(agent);const text=agent?t('Message {0}',agentName(agent)):t('Message agent (Bracketed Paste)…');if(input.placeholder!==text)input.placeholder=text;}
 function copyText(text,node){
  run(async()=>{
   // 클립보드 권한이 거부되면(일부 웹뷰) 선택 복사로 한 번 더 시도한다.
   let copied=false;if(globalThis.isSecureContext&&navigator.clipboard)copied=await navigator.clipboard.writeText(text).then(()=>true,()=>false);
   if(!copied){const area=el('textarea');area.value=text;area.setAttribute('readonly','');area.className='copy-buffer';document.body.append(area);area.select();const ok=document.execCommand('copy');area.remove();if(!ok)throw Error(t('Copy is not available in this browser'));}
   if(!node)return;
   if(ctx.icon&&String(node.className||'').includes('chat-copy')){
    node.replaceChildren(ctx.icon('check'));node.setAttribute('aria-label',t('Copied'));node.dataset.copied='true';
    setTimeout(()=>{node.replaceChildren(ctx.icon('copy'));node.setAttribute('aria-label',t('Copy'));delete node.dataset.copied;},1500);
   }else{
    const prev=node.textContent;node.textContent=t('Copied');node.dataset.copied='true';
    setTimeout(()=>{node.textContent=prev;delete node.dataset.copied;},1500);
   }
  });
 }
 // 오늘은 시각만, 다른 날은 날짜와 시각을 보인다(Orca 메시지 아래 표기).
 function formatTime(at){
  if(!at)return '';const date=new Date(/^\d+$/.test(at)?Number(at):at);if(Number.isNaN(date.getTime()))return '';
  const lang=M.i18n?.language,today=new Date().toDateString()===date.toDateString();
  return date.toLocaleString(lang,today?{hour:'numeric',minute:'2-digit'}:{month:'numeric',day:'numeric',hour:'numeric',minute:'2-digit'});
 }
 function messageMeta(item){
  const meta=el('footer',undefined,'chat-meta'),copy=button(t('Copy'),()=>copyText(item.text,copy),'icon-button chat-copy','copy');meta.append(copy);
  const when=formatTime(item.at);if(when){const time=el('time',when,'chat-time');time.setAttribute('datetime',new Date(/^\d+$/.test(item.at)?Number(item.at):item.at).toISOString());meta.append(time);}
  return meta;
 }
 // act owns product-level error presentation; local operations retain their own visible status.
 function run(fn){if(ctx.act)ctx.act(fn);else Promise.resolve().then(fn).catch(e=>ctx.error?.(e));}
 function updateComposer(sent){
  // Completion belongs to the shared draft version, including a newly mounted composer.
  if(sent&&(entry.draftVersion||0)===sent.version&&input.value===sent.text)input.value='';
  sizeInput();
  sendBtn.disabled=entry.sending||entry.photo?.status==='pending'||entry.photo?.status==='error';
  attachBtn.disabled=entry.sending||!!entry.photo;
  attachment.replaceChildren();
  const photo=entry.photo;if(!photo)return;
  if(photo.status==='ready'&&photo.prepared?.data){
   const img=el('img',undefined,'chat-photo-thumb'),src=`data:${photo.prepared.mime||'image/jpeg'};base64,${photo.prepared.data}`;
   img.alt='';img.src=src;img.setAttribute('alt','');img.setAttribute('src',src);
   attachment.append(img);
  }
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
 function readerAnchor(){if(following||!(feed.clientHeight>0))return null;const top=feed.getBoundingClientRect?.().top,node=Array.from(feed.children||[]).find(card=>card.getBoundingClientRect?card.getBoundingClientRect().bottom>top:(card.offsetTop||0)+(card.clientHeight||0)>feed.scrollTop);return node?{node,top:node.getBoundingClientRect?node.getBoundingClientRect().top:(node.offsetTop||0)-feed.scrollTop}:null;}
 function restoreReader(anchor){if(!following&&anchor&&anchor.node.parentElement){const top=anchor.node.getBoundingClientRect?anchor.node.getBoundingClientRect().top:(anchor.node.offsetTop||0)-feed.scrollTop;feed.scrollTop+=top-anchor.top;lastTop=feed.scrollTop;}}
 function renderItems(appendFrom){
  const anchor=readerAnchor();
  const incremental=appendFrom>0&&renderedItems===entry.items&&renderedCount===appendFrom;
  const wanted=new Set(),wantedRuns=new Set(),nodes=[];let run=incremental?tailRun:null;
  // Retain the actual question/details nodes so selection, write-in, focus and expansion survive deltas.
  for(let idx=incremental?appendFrom:0;idx<entry.items.length;idx++){const item=entry.items[idx];
   const key=itemKey(item,idx);wanted.add(key);
   let card=cards.get(key);
   if(!card){card=renderCard(item,key);card.dataset.chatId=key;cards.set(key,card);}else if(!sameItem(card._item,item)){const next=renderCard(item,key);card.replaceChildren(...Array.from(next.children));}card._item=item;
   if(item.kind==='tool_call'||item.kind==='tool_result'){
    if(!run){let group=toolRuns.get(key);if(!group){const details=el('details',undefined,'chat-tool-run'),summary=el('summary',undefined,'chat-tool-summary'),body=el('div',undefined,'chat-tool-items');rememberDetails(details,'run:'+key);details.append(summary,body);group={details,summary,body,pairs:new Map()};toolRuns.set(key,group);}run={...group,key,nodes:[],nodeSet:new Set(),steps:new Map(),calls:new Map(),pending:[],pairs:group.pairs,count:0,errors:0};wantedRuns.add(key);nodes.push(run.details);}
    // Pair results by the wire tool ID, with ordered fallback for older transcripts.
    const callId=item.call_id||item.tool_use_id||item.tool_id;let pair;
    if(item.kind==='tool_call'){
     pair=run.pairs.get(key)||{node:el('div',undefined,'chat-tool-pair'),call:item};run.pairs.set(key,pair);run.pending.push(pair);if(callId)run.calls.set(String(callId),pair);
     run.count++;const step=toolStep(item.tool_name);run.steps.set(step,(run.steps.get(step)||0)+1);
    }else{
     pair=callId?run.calls.get(String(callId)):run.pending[0];
     if(pair){const index=run.pending.indexOf(pair);if(index>=0)run.pending.splice(index,1);const summary=card.children?.[0]?.children?.[0];if(summary)summary.textContent=[t(item.is_error?'Tool error':'Tool output'),pair.call.tool_name,pair.call.title||pair.call.file_path].filter(Boolean).join(' · ');}
    }
    const node=pair?.node||card;if(pair){const at=pair.node.children?.[item.kind==='tool_call'?0:1];if(at!==card)pair.node.insertBefore(card,at||null);}
    if(!run.nodeSet.has(node)){run.nodeSet.add(node);run.nodes.push(node);const at=run.body.children?.[run.nodes.length-1];if(at!==node)run.body.insertBefore(node,at||null);}
    if(item.is_error)run.errors++;run.summary.textContent=[run.errors||!run.count?t(run.errors?'Tool error':'Tool activity'):'',...[...run.steps].map(([step,n])=>t(step,n))].filter(Boolean).join(' · ');
   }else{run=null;nodes.push(card);}
  }
  for(const [idx,node] of nodes.entries()){const at=incremental?null:feed.children?.[idx];if(at!==node){feed.insertBefore(node,at||null);resize?.observe(node);}}
  for(const [key,group] of toolRuns)if(!incremental){if(!wantedRuns.has(key)){group.details.remove();resize?.unobserve(group.details);toolRuns.delete(key);}else for(const [callKey,pair] of group.pairs)if(!wanted.has(callKey)){pair.node.remove();group.pairs.delete(callKey);}}
  for(const [key,card] of cards)if(!incremental&&!wanted.has(key)){card.remove();resize?.unobserve(card);cards.delete(key);}
  if(!incremental){for(const child of Array.from(feed.children||[]))if(child.className==='chat-empty')child.remove();for(const key of entry.disclosures?.keys()||[])if(!cards.has(key)&&!toolRuns.has(key.replace(/^run:/,'')))entry.disclosures.delete(key);}
  tailRun=run;renderedCount=entry.items.length;renderedItems=entry.items;
  if(!entry.items.length&&entry.loaded){
   const empty=el('div',undefined,'chat-empty');
   empty.append(el('p',t(entry.bound?'No chat turns recorded yet.':'Waiting for agent hook or transcript binding…'),'muted'));
   feed.append(empty);
  }
  const first=entry.items[0];
  const start=first?._pageStart||0;
  earlierBtn.hidden=!entry.pages.some(page=>page.start<start)&&!first?._clipped&&!(first&&start>0);
  earlierBtn.disabled=earlierPending;
  latestBtn.hidden=!entry.live;
  historyText.textContent=t(entry.live?'Reading earlier messages; live updates are paused.':'Showing up to 1000 messages. Load earlier messages to read more.');
  historyText.hidden=!entry.live;history.hidden=earlierBtn.hidden&&latestBtn.hidden;
  pinLatest();restoreReader(anchor);
 }
 function rememberDetails(node,key){node.open=entry.disclosures?.get(key)||false;node.addEventListener('toggle',()=>{entry.disclosures||=new Map();if(node.open)entry.disclosures.set(key,true);else entry.disclosures.delete(key);});}
 function lazyDetails(details,build){if(details.open)build();else details.addEventListener('toggle',()=>{if(details.open&&!details.dataset.loaded){details.dataset.loaded='true';build();}});if(details.open)details.dataset.loaded='true';}
 function renderCard(item,key){
  const kind=item.kind||'assistant',card=el('article',undefined,`chat-card chat-${kind}`);
  if(kind==='user'||kind==='assistant'){const role=el('header',kind==='user'?t('My request'):t('{0} reply',agentName(chatAgent())||t('Agent')),'chat-role');if(kind==='assistant'&&chatAgent()&&agentIcon)role.prepend(agentIcon(chatAgent()));card.append(role,renderRichText(el,item.text),messageMeta(item));}
  else if(kind==='reasoning'||kind==='tool_result'){
   const details=el('details',undefined,kind==='reasoning'?'chat-reasoning':'chat-tool-result');
   rememberDetails(details,key);details.append(el('summary',t(kind==='reasoning'?'Reasoning':item.is_error?'Tool error':'Tool output')));lazyDetails(details,()=>details.append(kind==='reasoning'?renderRichText(el,item.text):el('pre',item.text,'chat-code')));card.append(details);
  }else if(kind==='tool_call'){
   const details=el('details',undefined,'chat-tool-call'),head=el('summary',undefined,'chat-tool-head');head.append(el('span',item.tool_name||'tool','badge'),el('span',item.title||item.file_path||t('Tool call'),'chat-tool-title'));rememberDetails(details,key);details.append(head);
   if(item.file_path)details.append(button(t('Current diff'),()=>openDiff(item.file_path),'ghost small'));
   if(item.text&&item.text!==item.file_path)lazyDetails(details,()=>details.append(el('pre',item.text,'chat-tool-body')));card.append(details);
  }else if(kind==='question')renderQuestionCard(card,item);
  else if(kind==='permission'){
   card.append(el('header',`${t('Permission')} • ${item.title||item.tool_name||t('Approval')}`,'chat-role'),el('p',item.text,'chat-p'),el('p',t('Historical request. Confirm the current request in Term.'),'muted'),button(t('Confirm in Term'),()=>onSwitchTerm?.(),'ghost small'));
  }
  return card;
 }
 function renderQuestionCard(card,item){
  card.append(el('header',item.title?`${t('Question')} • ${item.title}`:t('Question'),'chat-role'),el('p',item.text,'chat-p'));
  const list=el('div',undefined,'chat-question-options'),selected=new Set(),buttons=[];
  const custom=el('input');custom.type='text';custom.value='';custom.placeholder=t('Other (write-in)…');custom.setAttribute('aria-label',t('Other answer notes'));
  const draftAnswer=()=>[...[...selected].map(i=>item.options?.[i]?.label).filter(Boolean),String(custom.value||'').trim()].filter(Boolean).join(', ');
  const copyBtn=button(t('Copy answer'),()=>{const ans=draftAnswer();if(ans)copyText(ans,copyBtn);},'secondary small');
  const syncAnswer=()=>{const has=Boolean(draftAnswer());copyBtn.hidden=!has;copyBtn.disabled=!has;};
  for(const [index,opt] of (item.options||[]).entries()){
   const b=button(opt.description?`${opt.label} — ${opt.description}`:opt.label,()=>{
    if(!item.multi_select)selected.clear();
    if(selected.has(index))selected.delete(index);else selected.add(index);
    buttons.forEach((b,i)=>{b.className=`${selected.has(i)?'primary':'ghost'} small chat-opt`;b.setAttribute('aria-pressed',String(selected.has(i)));});
    syncAnswer();
   },'ghost small chat-opt');b.setAttribute('aria-pressed','false');buttons.push(b);list.append(b);
  }
  custom.addEventListener('input',syncAnswer);syncAnswer();
  const actions=el('div',undefined,'chat-actions');actions.append(button(t('Confirm in Term'),()=>onSwitchTerm?.(),'ghost small'),copyBtn);
  card.append(list,custom,el('p',t('These are answer notes only. Confirm the current question and enter your answer in Term.'),'muted'),actions);
 }
 function openDiff(filePath){
  run(async()=>{
   const normalizePrefix=p=>String(p||'').replace(/^\/private(?=\/(?:tmp|var)(?:\/|$))/,'');
   const sess=state.sessions?.get(sessionId),cwd=normalizePrefix(sess?.cwd).replace(/\/+$/,''),path=normalizePrefix(filePath);
   const relPath=cwd&&path.startsWith(cwd+'/')?path.slice(cwd.length+1):filePath;
   const data=await ctx.rpc({action:'git',session:sessionId,path:relPath});
   if(disposed||ctx.signal?.aborted)return;
   ctx.sheet?.(t('Current diff')+`: ${filePath}`,[],undefined,body=>{
    const message=data?.diff_status==='outside_scope'?t('This file is outside the current workspace diff.'):t('No current changes for this file. This is a historical tool record; its earlier diff is not stored here.');
    body.append(el('pre',data?.patch||message,data?.patch?'chat-code':'muted'));
   });
  });
 }
 function updateStrip(){
  const sess=state.sessions?.get(sessionId),hidden=!['Waiting','Blocked'].includes(sess?.state),anchor=strip.hidden!==hidden?readerAnchor():null;stripBadge.textContent=t(sess?.state||'Unknown');stripBadge.dataset.state=sess?.state||'';syncAgent();
  // 헤더가 연결·모드를 보이므로 하단 줄은 사용자가 답해야 할 때만 띄운다.
  strip.hidden=hidden;
  stripText.textContent=sess?.lines?.at(-1)||(entry.bound?`${agentName(entry.agent)||'agent'} ${t('bound')}`:t('PTY live'));restoreReader(anchor);
 }
 function normalizeItems(res,start){return (res.items||[]).map((item,index)=>({...item,id:item.id||`${start}:${index}`,_pageStart:start,_retainedBytes:JSON.stringify(item).length*2}));}
 function clearTranscript(){entry.items=[];entry.retainedBytes=0;entry.disclosures?.clear();entry.pages=[];entry.live=null;entry.agent='';entry.bound=false;cards.clear();toolRuns.clear();feed.replaceChildren();resize?.disconnect();resize?.observe(feed);}
 function rememberPage(start,next){if(!(next>start))return;const pages=entry.pages,last=pages.at(-1);if(last&&start>last.start)pages.push({start,next});else if(!pages.some(p=>p.start===start)){let at=0;while(at<pages.length&&pages[at].start<start)at++;pages.splice(at,0,{start,next});}if(pages.length>pageLimit){const first=entry.items[0]?._pageStart??start;while(pages.length>pageLimit){if(pages[0].next<=first)pages.shift();else pages.pop();}}}
 function trimItems(fromStart){
  let bytes=entry.items.reduce((n,item)=>n+(item._retainedBytes||0),0),count=0;
  while(entry.items.length-count>1&&(entry.items.length-count>itemLimit||bytes>itemByteLimit)){const item=entry.items[fromStart?count:entry.items.length-1-count];bytes-=item._retainedBytes||0;count++;}
  entry.retainedBytes=bytes+(entry.live?.items.reduce((n,item)=>n+(item._retainedBytes||0),0)||0);
  if(!count)return false;
  if(fromStart){
   const removed=entry.items.splice(0,count),first=entry.items[0];
   // Re-read this page only when it actually contained an item that was clipped.
   if(first&&removed.at(-1)?._pageStart===first._pageStart)first._clipped=true;
  }
  else entry.items.length-=count;
  return true;
 }
 function showPollError(error){
  const anchor=readerAnchor();failures++;pollStatus.hidden=false;
  const when=lastSuccess?new Date(lastSuccess).toTimeString().slice(0,8):t('never');
  pollText.textContent=`${t('Chat history update failed')} (${failures}). ${t('Last updated')}: ${when}. ${t('Retry or check Term.')} ${String(error?.message||error||'')}`;restoreReader(anchor);
 }
 function sameItem(a,b){
  if(!a||!b)return false;
  if((b.id!==undefined&&a.id!==b.id)||a.kind!==b.kind||a.text!==b.text||a.title!==b.title||a.tool_name!==b.tool_name||a.tool_id!==b.tool_id||a.file_path!==b.file_path||a.is_error!==b.is_error||a.status!==b.status||a.multi_select!==b.multi_select)return false;
  const ao=a.options,bo=b.options;
  if(!!ao!==!!bo)return false;
  if(ao){if(ao.length!==bo.length)return false;for(let i=0;i<ao.length;i++)if(ao[i]?.label!==bo[i]?.label||ao[i]?.description!==bo[i]?.description)return false;}
  return true;
 }
 async function pollNow(initial=false){
  // 숨은 Chat도 첫 내용을 받을 때까지는 받아 둔다(실패 2회까지). 연결 전에 마운트돼 첫 프리페치를 놓쳐도 ready 뒤 update()가 다시 받아, Chat을 누른 첫 프레임에 내용이 있다.
  if(disposed||(!visible&&!initial&&(entry.loaded||failures>=2))||globalThis.document?.hidden||globalThis.document?.visibilityState==='hidden'||inFlight||earlierPending||entry.live||state.connection?.phase!=='ready')return;
  inFlight=true;
  try{
   // v0.1.37 이하 데몬은 latest를 모르는 필드로 거부한다. chat_tail이 없는 데몬이 그렇게 거부하면 처음부터 이어 읽는다(빈 화면으로 남기지 않는다).
   if(entry.noTail&&state.snapshot?.capabilities?.chat_tail===true)entry.noTail=false;
   const start=entry.offset;let latest=(!entry.initialized||refreshLatest)&&!entry.noTail,res=await ctx.rpc({action:'chat_poll',session:sessionId,offset:latest?0:start,...latest?{latest:true}:{}});
   if(disposed||earlierPending||entry.live||!res)return;
   let changed=entry.generation&&res.generation&&entry.generation!==res.generation;
   // Rebinding also opens at the tail. Keep the current paint until that window is ready.
   if(!latest&&!entry.noTail&&(res.reset||changed)){
    res=await ctx.rpc({action:'chat_poll',session:sessionId,offset:0,latest:true});
    if(disposed||earlierPending||entry.live||!res)return;
    latest=true;changed=true;
   }
   const rItems=res.items||[],sameTail=latest&&!changed&&!res.reset&&entry.generation===res.generation&&entry.items.length===rItems.length&&rItems.every((item,index)=>sameItem(entry.items[index],item));
   const reset=Boolean(res.reset||changed),metaChanged=entry.agent!==(res.agent||'')||entry.bound!==!!res.bound;
   const hadItems=entry.items.length,wasLoaded=entry.loaded,anchor=!sameTail&&(rItems.length||metaChanged)||!pollStatus.hidden?readerAnchor():null;
   if(reset){clearTranscript();following=true;}
   entry.generation=res.generation||entry.generation;entry.agent=res.agent||'';entry.bound=!!res.bound;
   entry.initialized=entry.bound;entry.loaded=true;refreshLatest=false;
   const effectiveStart=res.page_start??(res.reset?0:start);
   const incoming=normalizeItems(res,effectiveStart),before=entry.items.length;
   let reconciled=false;
   if(incoming.length){if(latest&&!reset&&!sameTail){const incomingIds=new Set(incoming.map(item=>item.id)),overlap=entry.items.findIndex(old=>incomingIds.has(old.id)),next=overlap>=0?[...entry.items.slice(0,overlap),...incoming]:incoming;reconciled=next.length!==entry.items.length||next.some((item,i)=>!sameItem(entry.items[i],item));if(reconciled)entry.items=next;}else if(!sameTail){const seen=new Set(entry.items.map(item=>item.id));entry.items.push(...incoming.filter(item=>!seen.has(item.id)));}rememberPage(effectiveStart,res.next_offset);}
   const trimmed=incoming.length&&trimItems(true);
   if(typeof res.next_offset==='number')entry.offset=res.next_offset;
   lastSuccess=entry.lastSuccess=Date.now();failures=0;pollStatus.hidden=true;
   if(reconciled||entry.items.length!==before||trimmed||metaChanged||!wasLoaded||reset&&(hadItems||!renderedItems))renderItems(!reset&&!trimmed&&!reconciled?before:undefined);updateStrip();restoreReader(anchor);pruneCache(state);
  }catch(error){if(disposed)return;if(!entry.noTail&&state.snapshot&&!state.cached&&state.snapshot.capabilities?.chat_tail!==true&&/invalid mobile authentication or frame/i.test(String(error?.message||error))){entry.noTail=true;retryWithoutTail=true;return;}showPollError(error);}finally{inFlight=false;if(retryWithoutTail){retryWithoutTail=false;pollNow(initial);}}
 }
 async function loadEarlier(){
  if(disposed||earlierPending||inFlight||!entry.items.length)return;
  earlierPending=true;following=false;renderItems();
  const anchor=Array.from(feed.children||[]).find(card=>(card.offsetTop||0)>=(feed.scrollTop||0))||feed.children?.[0];
  const oldTop=anchor?.offsetTop||0,oldScroll=feed.scrollTop||0,first=entry.items[0];
  const page=first._clipped?entry.pages.find(page=>page.start===first._pageStart):entry.pages.filter(page=>page.start<first._pageStart).at(-1);
  let start=page?.start;
  const end=page?.next??first._pageStart;
  if(start===undefined&&!end){earlierPending=false;renderItems();return;}
  try{
   const res=await ctx.rpc({action:'chat_poll',session:sessionId,offset:start??0,end_offset:end,...start===undefined?{latest:true}:{}});
   if(disposed)return;
   if(res.next_offset!==end)throw new Error(t('Transcript changed. Return to latest and retry.'));
   if(res.reset||(res.generation&&entry.generation&&res.generation!==entry.generation))throw new Error(t('Transcript changed. Return to latest and retry.'));
   start=res.page_start??start;rememberPage(start,res.next_offset);
   const items=normalizeItems(res,start),index=items.findIndex(item=>item.id===first.id);
   const prefix=index>=0?items.slice(0,index):items;
   if(!entry.live)entry.live={items:entry.items.slice(),offset:entry.offset};
   const seen=new Set(entry.items.map(item=>item.id));entry.items.unshift(...prefix.filter(item=>!seen.has(item.id)));trimItems(false);
   if(first._clipped)delete first._clipped;
   pollStatus.hidden=true;renderItems();
   feed.scrollTop=oldScroll+((anchor?.offsetTop||0)-oldTop);
  }catch(error){showPollError(error);}finally{earlierPending=false;if(!disposed)renderItems();}
 }
 function isVisible(){return visible&&!globalThis.document?.hidden&&globalThis.document?.visibilityState!=='hidden';}
 function schedule(){clearTimeout(pollTimer);pollTimer=null;if(!disposed&&isVisible())pollTimer=setTimeout(async()=>{await pollNow();schedule();},1500);}
 function visibilityChanged(){schedule();if(isVisible()){if(Date.now()-lastSuccess>3000)refreshLatest=true;pinLatest();pollNow();}}
 globalThis.document?.addEventListener?.('visibilitychange',visibilityChanged);
 entry.notifyComposer=updateComposer;
 renderItems();updateStrip();updateComposer();pollNow(true);schedule();
 return {insertText,pollNow,setVisible(next){const changed=visible!==!!next;visible=!!next;if(changed)visibilityChanged();},update(){updateStrip();updateComposer();pollNow();},unmount(){if(disposed)return;disposed=true;entry.mounts=Math.max(0,(entry.mounts||1)-1);globalThis.document?.removeEventListener?.('visibilitychange',visibilityChanged);resize?.disconnect();pruneCache(state);clearTimeout(pollTimer);pollTimer=null;if(entry.notifyComposer===updateComposer)entry.notifyComposer=null;}};
}
M.chat={mount:mountChat,pruneCache,forgetSession};
M.screens.chat=(root,ctx)=>{
 const session=ctx.state.sessions.get(ctx.route.sessionId);
 if(!session){root.append(ctx.el('p',(ctx.t||((s)=>s))('Session not found'),'empty'));return {};}
 const handle=mountChat(root,{ctx,sessionId:session.id,onSwitchTerm:()=>ctx.go(ctx.groupRoute?ctx.groupRoute(ctx.state?.groups?.get(session.groupKey),session.id):`/host/local/group/${encodeURIComponent(session.groupKey||'default')}/s/${session.id}`)});
 return {update:()=>handle.update(),cleanup:()=>handle.unmount()};
};
})();
