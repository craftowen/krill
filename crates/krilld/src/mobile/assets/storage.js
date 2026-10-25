// Metadata cache only. Credentials keep their existing localStorage durability contract.
(()=>{
 const M=globalThis.KrillMobile||={},DB='krill-mobile-metadata',VERSION=1,STORE='snapshots',LEGACY='krill-mobile-snapshot-v1',TTL=86400000,LIMIT=32;
 const SEED_PREFIX='krill-mobile-seed-v1:';
 const pick=(value,keys)=>Object.fromEntries(keys.filter(k=>['string','number','boolean'].includes(typeof value?.[k])||value?.[k]===null).map(k=>[k,value[k]]));
 const rows=(values,keys)=>Array.isArray(values)?values.slice(0,256).map(v=>pick(v,keys)):[];
 // Explicit allowlists keep transcripts, questions, approvals, credentials and future payloads out.
 function metadata(data){return {etag:null,host:pick(data?.host,['display_name','platform']),truncated:!!data?.truncated||['sessions','repositories','workspaces'].some(key=>Array.isArray(data?.[key])&&data[key].length>256),capabilities:{},
  sessions:rows(data?.sessions,['id','state','agent','title','foreground','cwd','exited','branch','added','deleted','repo_key','worktree_key','group_label','group_path','state_source','state_confidence','state_since_unix_ns']),
  repositories:rows(data?.repositories,['key','label','path']),workspaces:rows(data?.workspaces,['key','label','path','repo_key','pinned','branch','depth','primary'])};}
 const identity=(host,auth)=>auth?.device_id&&auth?.credential?{host,device:auth.device_id,scope:JSON.stringify([host,auth.device_id,MkCrypto.hex(MkCrypto.sha256(MkCrypto.enc.encode(auth.credential)))])}:null;
 function create({indexedDB,storage,timeout=1500,now=()=>Date.now(),broadcast=true}={}){
  try{indexedDB??=globalThis.indexedDB;}catch{}try{storage??=globalThis.localStorage;}catch{}
  const memory=new Map(),revisions=new Map(),identities=new Map(),lifetimes=new Map(),listeners=new Set();let connection=null,opening=null,closed=false,disabled=false,channel,writeBackoffUntil=0;
  try{if(broadcast&&globalThis.BroadcastChannel)channel=new BroadcastChannel(DB);}catch{}
  const revision=scope=>revisions.get(scope)||0,bump=scope=>revisions.set(scope,revision(scope)+1);
  const announce=message=>{try{channel?.postMessage(message);}catch{}};
  const fresh=record=>record?.schema===VERSION&&typeof record.at==='number'&&record.at<=now()&&now()-record.at<TTL;
  const validSnapshot=data=>data&&typeof data==='object'&&!Array.isArray(data)&&['sessions','repositories','workspaces'].every(key=>Array.isArray(data[key])&&data[key].length<=256)&&data.sessions.every(s=>Number.isSafeInteger(s?.id)&&s.id>0);
  function purgeSeed(host,keep=null){try{const key=SEED_PREFIX+host;if(keep===null||JSON.parse(storage?.getItem(key)||'null')?.scope!==keep)storage?.removeItem(key);}catch{try{storage?.removeItem(SEED_PREFIX+host);}catch{}}}
  function remember(record){memory.delete(record.scope);memory.set(record.scope,record);while(memory.size>LIMIT)memory.delete(memory.keys().next().value);}
  function open(){
   if(closed||disabled||!indexedDB)return Promise.resolve(null);if(connection)return Promise.resolve(connection);if(opening)return opening;
   opening=new Promise(resolve=>{let done=false,request;const finish=db=>{if(done){db?.close();return;}done=true;clearTimeout(timer);if(!db)disabled=true;connection=db;resolve(db);},timer=setTimeout(()=>finish(null),timeout);
    try{request=indexedDB.open(DB,VERSION);request.onupgradeneeded=()=>{const db=request.result;if(!db.objectStoreNames.contains(STORE)){const store=db.createObjectStore(STORE,{keyPath:'scope'});store.createIndex('host','host',{unique:false});}};
     request.onsuccess=()=>{const db=request.result;db.onversionchange=()=>{db.close();connection=null;disabled=true;};db.onclose=()=>{if(connection===db)connection=null;};if(closed)db.close();finish(closed?null:db);};request.onerror=request.onblocked=()=>finish(null);
    }catch{finish(null);}
   }).finally(()=>opening=null);return opening;
  }
  async function transaction(mode,run){const db=await open();if(!db||closed)return null;return new Promise(resolve=>{let done=false,tx,value=null;const finish=result=>{if(done)return;done=true;clearTimeout(timer);resolve(result);},timer=setTimeout(()=>{try{tx?.abort();}catch{}finish(null);},timeout);
   try{tx=db.transaction(STORE,mode);tx.oncomplete=()=>finish({value});tx.onerror=tx.onabort=()=>finish(null);run(tx.objectStore(STORE),v=>value=v);}catch{finish(null);}
  });}
  async function persist(record,allowed=()=>true){if(now()<writeBackoffUntil)return false;const result=await transaction('readwrite',(store,set)=>{if(!allowed()){set(false);return;}store.put(record);set(true);});if(!result)writeBackoffUntil=now()+60000;return result?.value===true;}
  function legacy(id){try{const raw=JSON.parse(storage?.getItem(LEGACY)||'null'),book=JSON.parse(storage?.getItem('krill-mobile-hosts-v1')||'null'),auth=book?.hosts?.find(h=>h.id===id.host);if(book?.active===id.host&&identity(id.host,auth)?.scope===id.scope&&raw?.device===id.device)return {schema:VERSION,...id,at:now(),data:metadata(raw.data)};}catch{}return null;}
  async function prune(host,keep=null){return transaction('readwrite',store=>{const request=store.openCursor();request.onsuccess=()=>{const cursor=request.result;if(!cursor)return;const row=cursor.value;if(!fresh(row)||row.host===host&&row.scope!==keep)cursor.delete();cursor.continue();};});}
  function purgeLegacy(host){try{const book=JSON.parse(storage?.getItem('krill-mobile-hosts-v1')||'null'),raw=JSON.parse(storage?.getItem(LEGACY)||'null'),auth=book?.hosts?.find(h=>h.id===host);if(book?.active===host&&raw?.device===auth?.device_id||[...identities.values()].some(id=>id.host===host&&id.device===raw?.device))storage?.removeItem(LEGACY);}catch{}}
  function invalidate(host,keep=null,send=true){let retired=false;for(const [scope,id] of identities)if(id.host===host&&scope!==keep){retired=true;bump(scope);if(lifetimes.has(scope))lifetimes.get(scope).active=false;memory.delete(scope);}purgeSeed(host,keep);if(keep===null||retired)purgeLegacy(host);for(const fn of listeners)fn(host,keep);if(send)announce({type:'invalidate',host,keep});return prune(host,keep);}
  if(channel)channel.onmessage=event=>{const m=event.data;if(m?.type==='invalidate'&&typeof m.host==='string')invalidate(m.host,m.keep,false);else if(m?.type==='changed'&&typeof m.scope==='string'){bump(m.scope);memory.delete(m.scope);}};
  const authChanged=()=>{let hosts;try{hosts=JSON.parse(storage?.getItem('krill-mobile-hosts-v1')||'null')?.hosts;}catch{}if(!Array.isArray(hosts))hosts=[];const allowed=new Set(hosts.map(h=>identity(h.id,h)?.scope));for(const [scope,record] of identities)if(!allowed.has(scope))invalidate(record.host,hosts.find(h=>h.id===record.host)?identity(record.host,hosts.find(h=>h.id===record.host))?.scope:null,false);};
  const storageEvent=e=>{if(e.key===null||e.key==='krill-mobile-hosts-v1')authChanged();};globalThis.addEventListener?.('storage',storageEvent);
  return {identity,metadata,onInvalidate(fn){listeners.add(fn);return ()=>listeners.delete(fn);},
   bind(host,auth){const id=identity(host,auth);if(!id)return null;let stopped=false,epoch=0;identities.set(id.scope,id);let lifetime=lifetimes.get(id.scope);if(!lifetime?.active){lifetime={active:true};lifetimes.set(id.scope,lifetime);}const start=revision(id.scope);if(!revisions.has(id.scope))revisions.set(id.scope,start);
    // Remove older auth generations; this read never grants server permissions.
    invalidate(host,id.scope);const valid=()=>!closed&&!stopped&&lifetime.active;
    // Seed the first synchronous render without waiting for IndexedDB. Keep the same TTL and allowlist.
    if(!memory.has(id.scope))try{const seed=JSON.parse(storage?.getItem(SEED_PREFIX+host)||'null'),record={schema:VERSION,...id,at:seed?.at,data:seed?.snapshot};if(seed?.scope===id.scope&&fresh(record)&&validSnapshot(record.data))remember({...record,data:metadata(record.data)});}catch{}
    return {scope:id.scope,peek(){if(!valid())return null;const record=memory.get(id.scope);return fresh(record)?metadata(record.data):null;},
     async hydrate(){if(!valid())return null;const before=epoch,rev=revision(id.scope),hit=memory.get(id.scope);if(fresh(hit)){await Promise.resolve();return valid()&&epoch===before&&revision(id.scope)===rev?metadata(hit.data):null;}
      const result=await transaction('readonly',(store,set)=>{const request=store.get(id.scope);request.onsuccess=()=>set(request.result);});
      if(!valid()||epoch!==before||revision(id.scope)!==rev)return null;
      let record=result?.value;if(!fresh(record)||record.scope!==id.scope||record.host!==id.host||record.device!==id.device)record=null;
      if(!record){record=legacy(id);if(record){remember(record);if(await persist(record,()=>valid()&&epoch===before&&revision(id.scope)===rev))try{storage?.removeItem(LEGACY);}catch{}}}
      if(!valid()||epoch!==before||revision(id.scope)!==rev)return null;if(record){remember(record);return metadata(record.data);}return null;
     },
     write(data){if(!valid())return Promise.resolve(false);epoch++;bump(id.scope);const record={schema:VERSION,...id,at:now(),data:metadata(data)};remember(record);const current=epoch;
      try{storage?.setItem(SEED_PREFIX+host,JSON.stringify({scope:id.scope,at:record.at,snapshot:record.data}));}catch{}
      return persist(record,()=>valid()&&epoch===current).then(ok=>{if(ok&&valid()&&epoch===current){announce({type:'changed',scope:id.scope});try{storage?.removeItem(LEGACY);}catch{}}return ok;});},
     close(){stopped=true;epoch++;},remove(){stopped=true;epoch++;bump(id.scope);memory.delete(id.scope);return invalidate(host);}
    };
   },removeHost:host=>invalidate(host),
   close(){closed=true;connection?.close();channel?.close();globalThis.removeEventListener?.('storage',storageEvent);listeners.clear();},
   async clear(){for(const scope of revisions.keys()){bump(scope);if(lifetimes.has(scope))lifetimes.get(scope).active=false;}memory.clear();try{for(let i=storage.length-1;i>=0;i--){const key=storage.key(i);if(key?.startsWith(SEED_PREFIX))storage.removeItem(key);}storage.removeItem(LEGACY);}catch{}const result=await transaction('readwrite',store=>store.clear());return !!result;}
  };
 }
 M.storage={create,identity,metadata};
})();
