
if(typeof document!=='undefined') (async()=>{
 const C=MkCrypto,$=id=>document.getElementById(id),decode=new TextDecoder();
 const element=(tag,text,cls)=>{const e=document.createElement(tag);if(text!==undefined)e.textContent=text;if(cls)e.className=cls;return e;};
 const button=(label,action,cls='')=>{const b=element('button',label,cls);b.type='button';b.addEventListener('click',()=>act(action));return b;};
 const error=e=>{$('notice').textContent=e?.message||String(e);$('notice').hidden=false;};
 const clearError=()=>{$('notice').hidden=true;};
 const variable=value=>{value=BigInt(value);const out=[];do{let b=Number(value&127n);value>>=7n;out.push(b|(value?128:0));}while(value);return Uint8Array.from(out);};
 const string=s=>{const data=C.enc.encode(s);return C.concat(variable(data.length),data);};
 const frame=(kind,body)=>{const h=new Uint8Array(5);new DataView(h.buffer).setUint32(0,body.length+1,true);h[4]=kind;return C.concat(h,body);};
 const reader=bytes=>{let at=0;return {u(){let n=0n,shift=0n;for(let i=0;i<10;i++){if(at>=bytes.length)throw Error('Truncated response');const b=bytes[at++];n|=BigInt(b&127)<<shift;if(!(b&128))return n;shift+=7n;}throw Error('Invalid response');},text(){const n=Number(this.u());if(n>bytes.length-at)throw Error('Truncated response');const text=decode.decode(bytes.slice(at,at+n));at+=n;return text;}};};
 const parameters=new URLSearchParams(location.search),fragment=new URLSearchParams(location.hash.slice(1));
 const ticket=parameters.get('pair'),secretText=fragment.get('key');
 // Capture the key once, then remove it from the address bar before any connection or asset action.
 history.replaceState({},'',location.pathname+location.search);
 let credentials;try{credentials=JSON.parse(localStorage.getItem('minikrill-mobile-v2')||'null');}catch{credentials=null;}
 let terminal;
 let socket,channel,queue=[],waiters=[],failed=null,selected=Number(parameters.get('session'))||null,snapshot=null,composing=false,closed=false;
 let generation=0,connected=false,connecting=false,reconnectTimer,retryDelay=1000;
 const next=()=>{if(queue.length)return Promise.resolve(queue.shift());if(failed)return Promise.reject(failed);return new Promise((resolve,reject)=>waiters.push({resolve,reject}));};
 const send=bytes=>socket.send(channel.seal(bytes));
 const nextPlain=async()=>channel.open(await next());
 let sequence=Promise.resolve();
 async function rpc(request){const current=generation;const operation=sequence.then(async()=>{if(!connected||current!==generation)throw Error('Disconnected. The action was not resent.');send(frame(0x45,C.concat(variable(7),string(JSON.stringify(request)))));const bytes=await nextPlain();if(bytes[4]!==0x65)throw Error('Unexpected daemon response');const r=reader(bytes.slice(5)),kind=Number(r.u());if(kind===4)throw Error(r.text());if(kind!==6)throw Error('Update the desktop for mobile workflows');const reply=JSON.parse(r.text());if(!reply.ok)throw Error(reply.error);return reply.data;});sequence=operation.catch(()=>{});return operation;}
 async function act(action){clearError();try{await action();}catch(e){error(e);}}
 function select(id){if(terminal?.session&&terminal.session!==id){terminal.close();$('terminal-panel').hidden=true;}selected=id;$('selected-session').textContent=`Session #${id}`;for(const card of document.querySelectorAll('.session-card'))card.classList.toggle('selected',Number(card.dataset.session)===id);}
 function render(data){
  snapshot=data;const sessions=data.sessions,needs=sessions.filter(s=>['Waiting','Blocked'].includes(s.state));
  $('connection').textContent='Encrypted · connected';$('connection').className='connection online';
  $('needs-count').textContent=String(needs.length+data.orchestration.gates.length+data.orchestration.questions.length);
  $('needs-you').hidden=!Number($('needs-count').textContent);$('urgent-items').replaceChildren();
  for(const s of needs){const row=element('div',undefined,'urgent-row');row.append(element('strong',`${s.agent||'Agent'} · #${s.id}`),button('Approve (Yes)',async()=>{await rpc({action:'approve',session:s.id,value:'y'});await refresh();},'approve'),button('Deny (No)',async()=>{await rpc({action:'approve',session:s.id,value:'n'});await refresh();},'deny'));$('urgent-items').append(row);}
  $('sessions').replaceChildren();
  for(const s of sessions){
   const displayState=s.exited?'Exited':s.state;
   const card=element('article',undefined,'session-card');card.dataset.session=s.id;
   const top=element('div',undefined,'card-heading');top.append(element('span',displayState.toLowerCase(),`status ${displayState.toLowerCase()}`),element('span',s.agent||'Shell','agent-badge'),element('span',`#${s.id}`,'session-id'));card.append(top);
   card.append(element('h2',s.cwd.split('/').filter(Boolean).pop()||`Session ${s.id}`),element('p',`${s.branch||'—'}  +${s.added||0} −${s.deleted||0}`,'branch'),element('pre',s.lines.join('\n'),'preview'));
   const actions=element('div',undefined,'card-actions');actions.append(button('Live terminal',()=>{select(s.id);terminal.open(s.id);document.getElementById('terminal-panel').scrollIntoView({block:'start'});},'primary'),button('Write prompt',()=>select(s.id)),button('Git diff',()=>showGit(s.id)),button('→ Codex',async()=>{await rpc({action:'handoff',session:s.id,agent:'codex'});await refresh();}),button('→ Claude',async()=>{await rpc({action:'handoff',session:s.id,agent:'claude'});await refresh();}));card.append(actions);$('sessions').append(card);
  }
  if(!selected||!sessions.some(s=>s.id===selected))selected=sessions[0]?.id||null;if(selected)select(selected);else $('selected-session').textContent='Select a session';
  renderOrchestration(data.orchestration);
 }
 function renderOrchestration(store){
  // Keep a worker reply and its IME composition stable while polling.
  if(document.activeElement?.matches('#orchestration input'))return;
  $('orchestration').replaceChildren();
  for(const gate of store.gates){const card=element('article',undefined,'gate-card');card.append(element('small','DECISION NEEDED','eyebrow'),element('h2',gate.question));for(const option of gate.options.length?gate.options:['Approve','Deny'])card.append(button(option,async()=>{await rpc({action:'gate_reply',id:gate.id,decision:option});await refresh();},'primary'));$('orchestration').append(card);const urgent=element('div',undefined,'urgent-row');urgent.append(element('strong',gate.question));for(const option of gate.options.length?gate.options:['Approve','Deny'])urgent.append(button(option,async()=>{await rpc({action:'gate_reply',id:gate.id,decision:option});await refresh();},'approve'));$('urgent-items').append(urgent);}
  for(const question of store.questions){const card=element('article',undefined,'gate-card');card.append(element('small','WORKER QUESTION','eyebrow'),element('h2',question.summary),element('p',question.body));const form=element('form'),input=element('input');input.placeholder='Reply to worker';input.required=true;const submit=element('button','Send reply');submit.type='submit';form.append(input,submit);form.addEventListener('submit',e=>{e.preventDefault();act(async()=>{await rpc({action:'question_reply',id:question.id,body:input.value});await refresh();});});card.append(form);$('orchestration').append(card);const urgent=element('div',undefined,'urgent-row');urgent.append(element('strong',question.summary),button('Reply',()=>switchTab('orchestration')));$('urgent-items').append(urgent);}
  for(const run of store.runs){const tasks=store.tasks.filter(t=>t.run_id===run.id),done=tasks.filter(t=>t.status==='completed').length,card=element('article',undefined,'run-card');card.append(element('small',run.status,'eyebrow'),element('h2',run.title),element('p',`${done} of ${tasks.length} tasks complete`));const progress=element('progress');progress.max=Math.max(1,tasks.length);progress.value=done;card.append(progress);for(const t of tasks){const task=element('div',undefined,'task');task.append(element('span',t.status,`status ${t.status}`),element('strong',t.title),element('small',t.deps.length?`Depends on ${t.deps.join(', ')}`:'Ready to run independently'));card.append(task);}$('orchestration').append(card);}
  if(!$('orchestration').children.length)$('orchestration').append(element('p','No active orchestration tasks.','empty'));
 }
 async function accounts(){const data=await rpc({action:'accounts'});$('accounts').replaceChildren();for(const a of data.accounts){const card=element('article',undefined,'account-card');card.append(element('small',a.provider,'eyebrow'),element('h2',a.label),element('p',a.email||a.id,'muted'));for(const [label,window] of [['5h',a.usage.five_hour],['7d',a.usage.seven_day]]){const row=element('div',undefined,'usage-row'),bar=element('progress');bar.max=100;bar.value=window?.percent||0;row.append(element('span',label),bar,element('span',window?`${window.percent}%`:'Unknown'));card.append(row);}if(a.active)card.append(element('span','Active account','active-account'));else card.append(button('Switch to this account',async()=>{await rpc({action:'switch_account',provider:a.provider.toLowerCase(),account_id:a.id});await accounts();},'primary'));$('accounts').append(card);}}
 async function showGit(session){const data=await rpc({action:'git',session,path:null});$('git-title').textContent=`${data.branch||'Git changes'} · +${data.added} −${data.deleted}`;$('git-files').replaceChildren();$('git-patch').textContent='Select a changed file to read its diff.';for(const path of data.files)$('git-files').append(button(path,async()=>{const diff=await rpc({action:'git',session,path});$('git-patch').textContent=diff.patch||'No textual diff.';}));$('git-dialog').showModal();}
 function switchTab(tab){for(const pane of ['sessions','orchestration','accounts'])$(pane).hidden=pane!==tab;for(const b of document.querySelectorAll('[data-tab]'))b.classList.toggle('active',b.dataset.tab===tab);if(tab==='accounts')act(accounts);}
 let refreshTimer;
 async function refresh(){const data=await rpc({action:'snapshot'});render(data);}
 function scheduleRefresh(){clearTimeout(refreshTimer);refreshTimer=setTimeout(async()=>{if(closed)return;try{await refresh();}catch(e){error(e);}scheduleRefresh();},2500);}
 $('prompt').addEventListener('compositionstart',()=>composing=true);$('prompt').addEventListener('compositionend',()=>composing=false);
 $('prompt').addEventListener('keydown',e=>{if(e.key==='Enter'&&(e.ctrlKey||e.metaKey)&&!composing&&!e.isComposing){e.preventDefault();$('composer').requestSubmit();}});
 $('composer').addEventListener('submit',e=>{e.preventDefault();if(composing)return;act(async()=>{if(!selected)throw Error('Select a session first');const text=$('prompt').value;if(!text.trim())return;const submit=$('send');submit.disabled=true;try{await rpc({action:'prompt',session:selected,text});$('prompt').value='';await refresh();}finally{submit.disabled=false;}});});
 for(const b of document.querySelectorAll('[data-tab]'))b.addEventListener('click',()=>switchTab(b.dataset.tab));
 $('close-git').addEventListener('click',()=>$('git-dialog').close());
 async function photo(file){
  if(!file||!selected)throw Error('Select a photo and a session first');if(file.size>20*1024*1024)throw Error('Photo must be smaller than 20 MB');
  const url=URL.createObjectURL(file),image=new Image();try{image.src=url;await image.decode();const canvas=document.createElement('canvas'),scale=Math.min(1,1600/Math.max(image.width,image.height));canvas.width=Math.max(1,Math.round(image.width*scale));canvas.height=Math.max(1,Math.round(image.height*scale));canvas.getContext('2d').drawImage(image,0,0,canvas.width,canvas.height);let blob;for(const quality of [.85,.65,.45]){blob=await new Promise(r=>canvas.toBlob(r,'image/jpeg',quality));if(blob&&blob.size<=512*1024)break;}if(!blob||blob.size>512*1024)throw Error('Photo is too large after resizing');const bytes=new Uint8Array(await blob.arrayBuffer());let binary='';for(const b of bytes)binary+=String.fromCharCode(b);await rpc({action:'photo',session:selected,mime:'image/jpeg',data:btoa(binary)});$('attachment-status').textContent='Photo attached to selected session';}finally{URL.revokeObjectURL(url);}
 }
 for(const id of ['photo','camera'])$(id).addEventListener('change',e=>act(async()=>{await photo(e.target.files[0]);e.target.value='';}));
 $('pick-photo').addEventListener('click',()=>$('photo').click());$('take-photo').addEventListener('click',()=>$('camera').click());
 function reconnect(){
  clearTimeout(reconnectTimer);if(!credentials||document.hidden)return;
  reconnectTimer=setTimeout(connect,retryDelay);retryDelay=Math.min(retryDelay*2,15000);
 }
 async function connect(){
  if(connecting)return;connecting=true;connected=false;closed=false;
  clearTimeout(reconnectTimer);clearTimeout(refreshTimer);await sequence;
  const current=++generation;queue=[];waiters=[];failed=null;
  try {
  if(!(ticket&&secretText)&&!credentials)throw Error('Scan a fresh QR code from desktop Settings → Mobile.');
  socket=new WebSocket(`${location.protocol==='https:'?'wss:':'ws:'}//${location.host}/ws`);socket.binaryType='arraybuffer';
  const active=socket;
  socket.addEventListener('message',e=>{if(current!==generation)return;const waiter=waiters.shift();if(waiter)waiter.resolve(e.data);else queue.push(e.data);});
  const terminate=()=>{if(current!==generation||closed)return;closed=true;connected=false;clearTimeout(refreshTimer);failed=Error('Disconnected. Pending actions were not resent.');$('connection').textContent='Disconnected';$('connection').className='connection';for(const w of waiters.splice(0))w.reject(failed);active.close();if(!connecting)reconnect();};socket.addEventListener('close',terminate);socket.addEventListener('error',terminate);
  await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});socket.addEventListener('close',()=>reject(failed),{once:true});});
  const challenge=JSON.parse(await next()),nonce=crypto.getRandomValues(new Uint8Array(32));if(challenge.version!==2)throw Error('Unsupported desktop protocol');
  const pairing=Boolean(parameters.has('pair')&&ticket&&secretText),secret=pairing?C.unbase64(secretText):C.sha256(C.unhex(credentials.credential));
  channel=new C.Channel(secret,C.concat(C.unhex(challenge.challenge),nonce));secret.fill(0);
  socket.send(JSON.stringify({token:pairing?ticket:null,device_id:pairing?null:credentials.device_id,client_nonce:C.hex(nonce)}));send(C.enc.encode(JSON.stringify({label:/iPhone|iPad/.test(navigator.userAgent)?'iPhone / iPad (Safari)':'Mobile browser'})));
  const authenticated=JSON.parse(decode.decode(await nextPlain()));if(authenticated.credential){credentials=authenticated;localStorage.setItem('minikrill-mobile-v2',JSON.stringify(credentials));}
  parameters.delete('pair');history.replaceState({},'',location.pathname+(parameters.size?'?'+parameters.toString():''));
  const hello=reader((await nextPlain()).slice(5));hello.u();hello.u();const features=hello.u();send(frame(2,C.concat(variable(1),variable(1),variable(features),variable(0),string('minikrill-mobile-pwa'))));
  if(!terminal){terminal=new MkLiveTerminal(credentials,error);globalThis.liveTerminal=terminal;}
  const initial=!snapshot;connected=true;await refresh();if(initial)clearError();retryDelay=1000;
  if(parameters.has('session')&&selected&&!terminal.ready)terminal.open(selected);scheduleRefresh();
  }catch(e){error(e);if(socket&&socket.readyState!==WebSocket.CLOSED)socket.close();}
  finally{connecting=false;if(closed)reconnect();}
 }
 // iOS may suspend or discard a socket while the page is hidden. Resume with the saved
 // credential and a fresh challenge; only snapshots are retried, never user actions.
 const resume=()=>{if(!document.hidden&&closed){retryDelay=1000;connect();}};
 document.addEventListener('visibilitychange',resume);globalThis.addEventListener('pageshow',resume);globalThis.addEventListener('online',resume);
 connect();
})();
