// Workflow transport. Pure JS crypto also works on Tailscale HTTP without SubtleCrypto.
(()=>{
 const M=globalThis.KrillMobile||= {},C=MkCrypto,decode=new TextDecoder(),FEATURES=(1n<<64n)-1n;
 const v=value=>{let n=BigInt(value),out=[];do{let b=Number(n&127n);n>>=7n;out.push(b|(n?128:0));}while(n);return Uint8Array.from(out);};
 const text=s=>{const bytes=C.enc.encode(s);return C.concat(v(bytes.length),bytes);};
 const frame=(kind,body)=>{const h=new Uint8Array(5);new DataView(h.buffer).setUint32(0,body.length+1,true);h[4]=kind;return C.concat(h,body);};
 const reader=bytes=>{let at=0;return {u(){let n=0n,shift=0n;for(let i=0;i<10;i++){if(at>=bytes.length)throw Error('Truncated response');const b=bytes[at++];n|=BigInt(b&127)<<shift;if(!(b&128))return n;shift+=7n;}throw Error('Invalid response');},text(){const n=Number(this.u());if(n>bytes.length-at)throw Error('Truncated response');const s=decode.decode(bytes.slice(at,at+n));at+=n;return s;}};};
 // Handoff: <device>.<credential>[.<rid>], validated by pwa.rs.
 const AUTH=/^([0-9a-f]{32,128})\.([0-9a-f]{64})(?:\.([0-9a-f]{32}))?$/,handoff=c=>{const token=[c?.device_id,c?.credential,c?.rid].filter(Boolean).join('.');return AUTH.test(token)?token:null;};
 // Pairing input is consumed once, before UI routing or networking.
 function bootstrap(){
  const match=/^\/m\/([0-9a-f]{16})\/$/.exec(location.pathname),fixed=!!match,hash=match?.[1];
  const parameters=new URLSearchParams(location.search),fragment=new URLSearchParams(location.hash.slice(1)),ticket=fragment.get('pair'),secret=fragment.get('key');
  const STORE='krill-mobile-v2',HOSTS='krill-mobile-hosts-v1',REMOVED='krill-mobile-removed-v1:';
  const removalKey=host=>REMOVED+C.hex(C.sha256(C.enc.encode(JSON.stringify([host.id,host.device_id,host.credential])))),removed=host=>{try{return !!host&&localStorage.getItem(removalKey(host))==='1';}catch{return false;}},validHost=h=>typeof h?.id==='string'&&typeof h.device_id==='string'&&typeof h.credential==='string';
  let saved,book;try{saved=JSON.parse(localStorage.getItem(STORE)||'null');}catch{saved=null;}
  try{book=JSON.parse(localStorage.getItem(HOSTS)||'null');}catch{}const hosts=Array.isArray(book?.hosts)?book.hosts.filter(h=>validHost(h)&&!removed(h)):[];
  let active=hosts.find(h=>h.id===book?.active)||hosts[0]||null;if(Array.isArray(book?.hosts))saved=active;
  // Home Screen relaunches reuse the saved device, even though the start URL contains a spent ticket.
  const pair=ticket&&secret&&!(fragment.has('install')&&saved?.credential)?{ticket,secret}:null;
  const auth=parameters.get('auth'),sensitive=['pair','key','t','r','install'],consumed=sensitive.some(key=>fragment.has(key));for(const key of [...sensitive,'auth'])parameters.delete(key);
  history.replaceState({},'',location.pathname+(parameters.size?'?'+parameters:'')+(consumed?'':location.hash));
  let credentials=pair?null:saved,directOrigin=location.origin,destinationFixed=fixed,pairingPending=!!pair;
  const hostId=c=>c.rid?'relay:'+c.rid:c.url?'tunnel:'+c.url:'direct:'+(c.origin||location.origin);
  let updateHandler=null,updatePending=false;const hostListeners=new Set();
  let storageFailed=false;const persist=()=>{try{const latest=JSON.parse(localStorage.getItem(HOSTS)||'null'),live=hosts.filter(h=>!removed(h));for(const host of latest?.hosts||[])if(validHost(host)&&!removed(host)&&!live.some(h=>h.id===host.id))live.push(host);hosts.splice(0,hosts.length,...live);localStorage.setItem(HOSTS,JSON.stringify({active:active&&!removed(active)?active.id:null,hosts}));storageFailed=false;}catch{storageFailed=true;}};
  const stillSaved=host=>{if(!host||removed(host))return false;try{const latest=JSON.parse(localStorage.getItem(HOSTS)||'null');if(Array.isArray(latest?.hosts))return latest.hosts.some(h=>h.id===host.id&&h.device_id===host.device_id&&h.credential===host.credential);}catch{}return true;};
  globalThis.addEventListener?.('storage',event=>{if(pairingPending||event.key!==null&&event.key!==HOSTS)return;let book;try{book=JSON.parse(localStorage.getItem(HOSTS)||'null');}catch{return;}const next=Array.isArray(book?.hosts)?book.hosts.filter(h=>validHost(h)&&!removed(h)):[];hosts.splice(0,hosts.length,...next);active=hosts.find(h=>h.id===active?.id)||hosts.find(h=>h.id===book?.active)||hosts[0]||null;saved=credentials=active;rid=active?.rid;url=active?.url;directOrigin=active?.origin||location.origin;destinationFixed=active?.fixed??fixed;useRelay=(credentials?.preferred_route==='relay'||!validUrl(url))&&validRid(rid);for(const fn of hostListeners)fn();});
  // Preserve the legacy credential during migration.
  if(saved&&!active){active={...saved,id:hostId(saved),origin:location.origin,fixed,name:'Desktop'};hosts.push(active);try{persist();}catch{}}
  if(active){directOrigin=active.origin||location.origin;destinationFixed=active.fixed??fixed;}
  if(pair){directOrigin=location.origin;destinationFixed=fixed;}
  // Seed empty iOS Home Screen storage; never overwrite a saved pairing.
  const seed=AUTH.exec(auth||'');if(!pair&&!saved&&seed&&(!fixed||seed[3])){credentials={device_id:seed[1],credential:seed[2],...(fixed&&{rid:seed[3],url:null,hash})};try{localStorage.setItem(STORE,JSON.stringify(credentials));}catch{}}
  // A link without both pairing secrets cannot replace a saved destination.
  let rid=pair?fragment.get('r'):credentials?.rid;
  let url=pair?(fragment.get('t')?'https://'+fragment.get('t'):null):credentials?.url;
  const validUrl=value=>typeof value==='string'&&/^https:\/\/[a-z0-9-]+\.trycloudflare\.com$/.test(value),validRid=value=>typeof value==='string'&&/^[0-9a-f]{32}$/.test(value);
  let useRelay=(credentials?.preferred_route==='relay'||!validUrl(url))&&validRid(rid);
  const relayUrl=()=>`${location.protocol==='https:'?'wss:':'ws:'}//${location.host}/r/${rid}?v=${hash}`;
  navigator.serviceWorker?.register('/sw.js').catch(()=>{});
  return {parameters,pair,credentials,fixed,replacing:!!(pair&&saved),get storageFailed(){return storageFailed;},
   forHost(id){
    const found=hosts.find(h=>h.id===id&&stillSaved(h));if(!found)throw Error('Desktop is no longer saved.');const source={...found};
    let auth={...source},peerUrl=source.url,relay=(source.preferred_route==='relay'||!validUrl(peerUrl))&&validRid(source.rid);
    const current=()=>hosts.find(h=>h.id===id&&stillSaved(h)),peerRid=source.rid,peerFixed=source.fixed??fixed;
    const save=patch=>{const host=current();if(!host||host.device_id!==auth.device_id||host.credential!==auth.credential)return false;if(Object.entries(patch).every(([key,value])=>host[key]===value))return true;Object.assign(host,patch);auth={...host};if(active?.id===id){active=saved=credentials=host;try{localStorage.setItem(STORE,JSON.stringify(host));}catch{storageFailed=true;}}persist();return true;};
    return {
     get credentials(){return auth;},
     socketUrl(){if(!peerFixed){const origin=new URL(source.origin||location.origin);return `${origin.protocol==='https:'?'wss:':'ws:'}//${origin.host}/ws`;}if(relay&&validRid(peerRid))return `${location.protocol==='https:'?'wss:':'ws:'}//${location.host}/r/${peerRid}?v=${hash}`;if(validUrl(peerUrl))return `wss://${peerUrl.slice(8)}/ws?v=${hash}`;throw Error('Desktop address is unavailable.');},
     authenticated(reply){if(reply.device_id&&reply.device_id!==source.device_id)throw Error('Desktop identity changed.');save({...(reply.credential?{credential:reply.credential}:{}),url:peerUrl,preferred_route:peerFixed?(relay?'relay':'tunnel'):'direct'});return auth;},
     describeHost(info){const patch={};if(info?.display_name)patch.name=info.display_name;if(info?.platform)patch.platform=info.platform;save(patch);},
     fallback(attempted){if(peerFixed&&attempted===this.socketUrl()){if(!relay&&validRid(peerRid))relay=true;else if(relay&&validUrl(peerUrl))relay=false;}},
     async locate({isCurrent=()=>true,signal}={}){if(!peerFixed||!validRid(peerRid))return false;const controller=new AbortController(),abort=()=>controller.abort(),timeout=setTimeout(abort,5000);signal?.addEventListener('abort',abort,{once:true});if(signal?.aborted)abort();try{const response=await fetch('/r/'+encodeURIComponent(peerRid),{cache:'no-store',signal:controller.signal});if(!response.ok||controller.signal.aborted||!isCurrent()||!current())return false;const found=await response.json();if(controller.signal.aborted||!isCurrent())return false;if(typeof found.hash==='string'&&/^[0-9a-f]{16}$/.test(found.hash)&&(validUrl(found.url)||found.url===''||found.relay)){peerUrl=validUrl(found.url)?found.url:null;relay=!peerUrl;save({url:peerUrl});}}catch{}finally{clearTimeout(timeout);signal?.removeEventListener('abort',abort);}return false;}
    };
   },
   onHostsChanged(fn){hostListeners.add(fn);return ()=>hostListeners.delete(fn);},
   reloadHosts(){let latest;try{latest=JSON.parse(localStorage.getItem(HOSTS)||'null');}catch{return false;}const live=(Array.isArray(latest?.hosts)?latest.hosts:[]).filter(h=>validHost(h)&&!removed(h));hosts.splice(0,hosts.length,...live);const next=hosts.find(h=>h.id===active?.id)||hosts.find(h=>h.id===latest?.active)||hosts[0]||null;active=saved=credentials=next;rid=next?.rid;url=next?.url;directOrigin=next?.origin||location.origin;destinationFixed=next?.fixed??fixed;useRelay=(credentials?.preferred_route==='relay'||!validUrl(url))&&validRid(rid);updatePending=false;return true;},
   profiles(){return hosts.filter(h=>!removed(h)).map(h=>({id:h.id,name:h.alias||h.name||'Desktop',platform:h.platform,endpoint:h.fixed?(h.rid?'Relay · '+h.rid.slice(0,8):h.url):h.origin,active:h.id===active?.id}));},
   selectHost(id){const host=hosts.find(h=>h.id===id&&stillSaved(h));if(!host)throw Error('Desktop is no longer saved.');updatePending=false;pairingPending=false;active=host;saved=credentials=host;rid=host.rid;url=host.url;directOrigin=host.origin||location.origin;destinationFixed=host.fixed??fixed;useRelay=(credentials?.preferred_route==='relay'||!validUrl(url))&&validRid(rid);persist();try{localStorage.setItem(STORE,JSON.stringify(host));}catch{storageFailed=true;}return credentials;},
   renameHost(id,value){const host=hosts.find(h=>h.id===id);if(!host)throw Error('Desktop is no longer saved.');const name=String(value).trim();if(!name||name.length>80)throw Error('Enter a desktop name (1–80 characters).');const previous=host.alias;host.alias=name;persist();if(storageFailed){if(previous===undefined)delete host.alias;else host.alias=previous;throw Error('Could not save desktop name.');}},
   forgetHost(id){const at=hosts.findIndex(h=>h.id===id);if(at<0)return;const selected=active?.id===id,remaining=hosts.filter(h=>h.id!==id&&!removed(h)),next=selected?remaining[0]||null:active,key=removalKey(hosts[at]);let oldBook,oldSaved,oldRemoved;
    try{oldBook=localStorage.getItem(HOSTS);oldSaved=localStorage.getItem(STORE);oldRemoved=localStorage.getItem(key);localStorage.setItem(key,'1');localStorage.setItem(HOSTS,JSON.stringify({active:next?.id||null,hosts:remaining}));if(selected){if(next)localStorage.setItem(STORE,JSON.stringify(next));else localStorage.removeItem(STORE);}}
    catch{try{if(oldBook!=null)localStorage.setItem(HOSTS,oldBook);else localStorage.removeItem(HOSTS);if(oldSaved!=null)localStorage.setItem(STORE,oldSaved);if(oldRemoved!=null)localStorage.setItem(key,oldRemoved);else localStorage.removeItem(key);}catch{}storageFailed=true;throw Error('Could not remove desktop. Try again.');}
    hosts.splice(at,1);storageFailed=false;if(selected){updatePending=false;active=saved=credentials=next;rid=next?.rid;url=next?.url;directOrigin=next?.origin||location.origin;destinationFixed=next?.fixed??fixed;useRelay=(credentials?.preferred_route==='relay'||!validUrl(url))&&validRid(rid);}return next?.id||null;
   },
   describeHost(host){if(!stillSaved(active)||!credentials)return;let changed=false;for(const [key,value] of [['name',host?.display_name],['platform',host?.platform]])if(value&&active[key]!==value){active[key]=value;changed=true;}if(changed)persist();},
   restoreSaved(){pairingPending=false;updatePending=false;credentials=saved;directOrigin=active?.origin||location.origin;destinationFixed=active?.fixed??fixed;rid=saved?.rid;url=saved?.url;useRelay=(credentials?.preferred_route==='relay'||!validUrl(url))&&validRid(rid);if(fixed&&/^[0-9a-f]{16}$/.test(saved?.hash||'')&&saved.hash!==hash){if(updateHandler)updateHandler();else location.replace('/m/'+saved.hash+'/');}return credentials;},
   socketUrl(){if(!destinationFixed){const origin=new URL(directOrigin);return `${origin.protocol==='https:'?'wss:':'ws:'}//${origin.host}/ws`;}if(useRelay&&validRid(rid))return relayUrl();if(validUrl(url))return `wss://${url.slice(8)}/ws?v=${hash}`;if(validRid(rid))return relayUrl();throw Error('Scan a fresh QR code from desktop Settings → Mobile.');},
   // use() adopts reachable codes; null opens another origin or app version.
   adopt(href){
    const NOT='Not a Krill pairing code. Scan the QR code in desktop Settings → Mobile.';let u;try{u=new URL(href);}catch{throw Error(NOT);}
    const f=new URLSearchParams(u.hash.slice(1)),ticket=f.get('pair'),secret=f.get('key'),version=/^\/m\/([0-9a-f]{16})\/?$/.exec(u.pathname)?.[1];
    if(!/^[0-9a-f]+$/.test(ticket||'')||!secret||!/^https?:$/.test(u.protocol))throw Error(NOT);
    if(!fixed)return u.origin===location.origin?{ticket,secret,use(){pairingPending=true;updatePending=false;destinationFixed=false;directOrigin=u.origin;}}:null;
    if(u.protocol==='https:'&&!version&&!f.has('t')&&!f.has('r')&&!validUrl(u.origin)&&!u.username&&!u.password)return null;
    if(u.origin===location.origin&&version&&version!==hash)return null;
    const t=f.get('t'),r=f.get('r'),next=t?'https://'+t:validUrl(u.origin)?u.origin:null;
    if(!validUrl(next)&&!validRid(r))throw Error(u.protocol==='http:'?'This direct code cannot be opened here. Choose Relay server in desktop Settings → Mobile, then tap New QR code.':NOT);
    return {ticket,secret,use(){pairingPending=true;updatePending=false;destinationFixed=true;url=validUrl(next)?next:null;rid=validRid(r)?r:null;useRelay=!url&&!!rid;}};
   },
   authenticated(auth){credentials=auth.credential?auth:credentials;if(destinationFixed)credentials={...credentials,rid,url,hash,preferred_route:useRelay?'relay':'tunnel'};
    if(!credentials)return null;const id=hostId({...credentials,origin:directOrigin}),existing=hosts.find(h=>h.id===id);active={...existing,...credentials,id,origin:directOrigin,fixed:destinationFixed,name:existing?.name||'Desktop'};if(existing)hosts[hosts.indexOf(existing)]=active;else hosts.push(active);if(removed(active))throw Error('Desktop is no longer saved.');saved=credentials=active;persist();try{localStorage.setItem(STORE,JSON.stringify(credentials));}catch{storageFailed=true;}pairingPending=false;return credentials;},
   fallback(attempted){if(!destinationFixed)return;try{if(attempted===this.socketUrl()){if(!useRelay&&validRid(rid))useRelay=true;else if(useRelay&&validUrl(url))useRelay=false;}}catch{}},
   setUpdateHandler(handler){updateHandler=handler;},
   get updatePending(){return updatePending;},
   clearUpdatePending(){updatePending=false;},
   updateIdentity(){return fixed&&destinationFixed&&credentials&&active?JSON.stringify([active.id,credentials.device_id,rid||url]):null;},
   async probeUpdate({signal}={}){
    const identity=this.updateIdentity();if(!identity||!validRid(rid))return null;
    const controller=new AbortController(),abort=()=>controller.abort(),timeout=setTimeout(abort,5000);
    if(signal?.aborted){clearTimeout(timeout);return null;}signal?.addEventListener('abort',abort,{once:true});
    try{
     const response=await fetch('/r/'+encodeURIComponent(rid),{cache:'no-store',signal:controller.signal});
     if(!response.ok||controller.signal.aborted||identity!==this.updateIdentity())return null;
     const found=await response.json();
     if(controller.signal.aborted||identity!==this.updateIdentity()||typeof found.hash!=='string'||!/^[0-9a-f]{16}$/.test(found.hash)||!(validUrl(found.url)||found.url===''||found.relay))return null;
     return {identity,hash:found.hash,rid,url:validUrl(found.url)?found.url:null};
    }finally{clearTimeout(timeout);signal?.removeEventListener('abort',abort);}
   },
   commitUpdate(found){
    if(found.identity!==this.updateIdentity()||found.rid!==rid||!/^[0-9a-f]{16}$/.test(found.hash))return false;
    let oldBook,oldSaved,writtenBook,writtenSaved;
    try{
     oldBook=localStorage.getItem(HOSTS);oldSaved=localStorage.getItem(STORE);
     const book=JSON.parse(oldBook||'null'),stored=JSON.parse(oldSaved||'null');
     const latest=book?.hosts?.find(h=>h.id===active.id);
     if(book?.active!==active.id||latest?.device_id!==credentials.device_id||latest?.credential!==credentials.credential||stored?.device_id!==credentials.device_id||stored?.credential!==credentials.credential)return false;
     const next={...latest,hash:found.hash,url:found.url,rid},nextHosts=book.hosts.map(h=>h.id===active.id?next:h),bookText=JSON.stringify({...book,active:next.id,hosts:nextHosts}),savedText=JSON.stringify(next);
     if(localStorage.getItem(HOSTS)!==oldBook||localStorage.getItem(STORE)!==oldSaved)return false;
     writtenBook=bookText;localStorage.setItem(HOSTS,bookText);writtenSaved=savedText;localStorage.setItem(STORE,savedText);
     if(localStorage.getItem(HOSTS)!==bookText||localStorage.getItem(STORE)!==savedText)throw Error('Update destination not saved');
     hosts.splice(0,hosts.length,...nextHosts);active=saved=credentials=next;url=next.url;useRelay=(credentials?.preferred_route==='relay'||!validUrl(url))&&validRid(rid);storageFailed=false;return true;
    }catch{storageFailed=true;try{if(oldBook!=null&&localStorage.getItem(HOSTS)===writtenBook)localStorage.setItem(HOSTS,oldBook);if(oldSaved!=null&&localStorage.getItem(STORE)===writtenSaved)localStorage.setItem(STORE,oldSaved);}catch{}return false;}
   },
   // Cancelled discovery cannot replace a newer destination.
   async locate({isCurrent=()=>true,signal}={}){
    if(!destinationFixed||!validRid(rid))return false;
    const requestRid=rid,controller=new AbortController(),abort=()=>controller.abort(),current=()=>!controller.signal.aborted&&!signal?.aborted&&isCurrent()&&rid===requestRid;
    if(!current())return false;signal?.addEventListener('abort',abort,{once:true});const timeout=setTimeout(abort,5000);
    try{const response=await fetch('/r/'+encodeURIComponent(requestRid),{cache:'no-store',signal:controller.signal});if(!current())return false;if(response.ok){const found=await response.json();if(!current())return false;if(typeof found.hash==='string'&&/^[0-9a-f]{16}$/.test(found.hash)&&(validUrl(found.url)||found.url===''||found.relay)){
     if(validUrl(found.url)){url=found.url;useRelay=false;}else{url=null;useRelay=true;}
     if(found.hash!==hash&&updateHandler){updatePending=true;updateHandler();return false;}
     if(credentials&&stillSaved(active)){credentials={...credentials,rid,url,hash:found.hash};if(active){Object.assign(active,credentials);saved=active;persist();}localStorage.setItem(STORE,JSON.stringify(credentials));}
     if(found.hash!==hash){location.replace('/m/'+found.hash+'/'+location.search+location.hash);return true;}
    }}}catch{}finally{clearTimeout(timeout);signal?.removeEventListener('abort',abort);}
    return false;
   }
  };
 }
 M.transport={bootstrap,handoff,pool({endpoint,onChange=()=>{},create}={}){
  const entries=new Map(),cache=M.storage?.create();let closed=false;const removeEntry=(entry,forget=false)=>{entry.cache?.close();entry.transport.close();if(forget)cache?.removeHost(entry.id);};
  const notify=entry=>{if(!closed&&entries.get(entry.id)===entry)onChange(entry);};
  const api={
   sync(){if(closed)return;const profiles=endpoint.profiles(),live=new Set(profiles.map(h=>h.id));for(const [id,entry] of entries)if(!live.has(id)){entries.delete(id);removeEntry(entry,true);}
    for(const profile of profiles){const peer=endpoint.forHost(profile.id),auth=peer.credentials,key=JSON.stringify([auth.device_id,auth.credential]);let entry=entries.get(profile.id);if(entry?.key===key){entry.profile=profile;continue;}if(entry){entries.delete(profile.id);removeEntry(entry,true);}
     entry={id:profile.id,key,profile,endpoint:peer,snapshot:null,cached:false,cache:cache?.bind(profile.id,auth),connection:{phase:'connecting',message:'Connecting…'},transport:null};entries.set(profile.id,entry);
     const cached=entry.cache?.peek();if(cached){entry.snapshot=cached;entry.cached=true;}entry.cache?.hydrate().then(data=>{if(data&&entries.get(entry.id)===entry&&(!entry.snapshot||entry.cached)){entry.snapshot=data;entry.cached=true;notify(entry);}});
     entry.transport=(create||M.transport.create)({endpoint:peer,credentials:auth,warmTerminal:false,onAuthenticated:()=>notify(entry),onSnapshot:data=>{if(entries.get(entry.id)!==entry)return;const current=peer.credentials,nextKey=JSON.stringify([current.device_id,current.credential]);if(!entry.cache||entry.key!==nextKey){entry.cache?.close();entry.cache=cache?.bind(entry.id,current);entry.key=nextKey;}entry.snapshot=data;entry.cached=false;entry.cache?.write(data);peer.describeHost(data.host);notify(entry);},onStatus:status=>{entry.connection=status;if(status.blocked){entry.cache?.remove();entry.cache=null;entry.snapshot=null;entry.cached=false;}notify(entry);},onError:()=>notify(entry)});
     entry.transport.connect();
    }
   },
   get:id=>entries.get(id),entries:()=>[...entries.values()],owns:transport=>[...entries.values()].some(e=>e.transport===transport),
   resume(stale){for(const entry of entries.values())entry.transport.resume(stale);},
   get updateBusy(){return [...entries.values()].some(e=>e.transport.updateBusy);},
   close(){closed=true;unsubscribe?.();for(const entry of entries.values())removeEntry(entry);entries.clear();cache?.close();}
  };const unsubscribe=cache?.onInvalidate((host,keep)=>{const entry=entries.get(host);if(entry&&entry.cache?.scope!==keep){entry.cache?.close();entry.cache=null;entry.snapshot=null;entry.cached=false;notify(entry);}});return api;
 },create(options){
  let pendingRpc=0,uncertainMutation=false;const latestReads=new Map(),isRead=request=>['snapshot','chat_poll','git','accounts'].includes(request?.action);
  let credentials=options.credentials,pair=options.pair,socket,channel,generation=0,connected=false,connecting=false,stopped=false,failed,queue=[],waiters=[],serial=Promise.resolve(),refreshing,timer,retryTimer,resumeTimer,connectController,warm=null,terminalActive=options.warmTerminal!==false,timing=null,retryDelay=1000,last=null,authFailures=0,retryNotBefore=0,authBlocked=false,retryCount=0,nextRetryAt=0;
  const socketUrl=()=>options.endpoint?options.endpoint.socketUrl():options.url;
  const clock=()=>globalThis.performance?.now?.()??Date.now(),mark=name=>{if(timing)timing[name]=Math.max(0,clock()-timing.started);},timings=()=>timing?Object.fromEntries(Object.entries(timing).filter(([key])=>key!=='started')):null;
  let state={phase:'disconnected',message:'',generation,retryAt:0,blocked:false};
  const status=(phase,message)=>{state={phase,message,generation,retryAt:Math.max(retryNotBefore,nextRetryAt),retryCount,blocked:authBlocked,timings:timings()};options.onStatus?.(state);};
  function deadline(operation,ms,message,current=generation){let timeout;return Promise.race([operation,new Promise((_,reject)=>{timeout=setTimeout(()=>{const error=Error(message);error.transport=true;reject(error);if(current===generation)disconnect(error);},ms);})]).finally(()=>clearTimeout(timeout));}
  const next=()=>queue.length?Promise.resolve(queue.shift()):failed?Promise.reject(failed):new Promise((resolve,reject)=>waiters.push({resolve,reject}));
  const send=bytes=>socket.send(channel.seal(bytes));
  const plain=async()=>channel.open(await next());
  function clearWarm(close=false){if(!warm)return;const w=warm;warm=null;w.cleanup();if(close)try{w.socket.close();}catch{}}
  // 미리 인증한 WS 하나를 터미널 탭 전환에 재사용한다. 첫 IPC 요청 대기는 5분.
  // force는 곧 붙을 터미널(새 세션) 하나를 위해 warmTerminal:false인 연결에서도 한 번만 미리 연다.
  function warmTerminal(force=false){if(!force&&(!terminalActive||options.warmTerminal===false)||!connected||!credentials||stopped||warm)return;let ws;try{ws=new WebSocket(socketUrl());}catch{return;}ws.binaryType='arraybuffer';const messages=[],auth=credentials,onMessage=e=>{if(warm?.socket!==ws)return;if(!warm.channel){try{const challenge=JSON.parse(e.data),nonce=crypto.getRandomValues(new Uint8Array(32));if(challenge.version!==2)throw Error('Unsupported terminal protocol');const ch=new C.Channel(C.sha256(C.unhex(auth.credential)),C.concat(C.unhex(challenge.challenge),nonce));ws.send(JSON.stringify({device_id:auth.device_id,token:null,client_nonce:C.hex(nonce)}));ws.send(ch.seal(C.enc.encode('{"label":"Live terminal"}')));ws.send(ch.seal(frame(2,C.concat(v(1),v(1),v((1n<<56n)|1n|4n|4096n|8192n),v(0),text('krill-mobile-grid')))));warm.channel=ch;}catch{clearWarm(true);}return;}messages.push(e.data);},onEnd=()=>{if(warm?.socket===ws)warm=null;cleanup();},cleanup=()=>{ws.removeEventListener?.('message',onMessage);ws.removeEventListener?.('close',onEnd);ws.removeEventListener?.('error',onEnd);};ws.addEventListener('message',onMessage);ws.addEventListener('close',onEnd);ws.addEventListener('error',onEnd);warm={socket:ws,credentials:auth,channel:null,messages,cleanup};}
  function retry(delay=retryDelay){clearTimeout(retryTimer);if(options.endpoint?.updatePending){nextRetryAt=0;return;}if(stopped||authBlocked||(!credentials&&!pair))return;const wait=Math.max(delay,retryNotBefore-Date.now());nextRetryAt=Date.now()+wait;status('disconnected',retryNotBefore>Date.now()?'Authentication retry is cooling down. Wait, or scan a fresh QR code.':failed?.message||'Reconnecting…');retryTimer=setTimeout(connect,wait);retryDelay=Math.min(retryDelay*2,15000);}
  function disconnect(error){if(failed)return;const wasConnected=connected;failed=error;connected=false;clearTimeout(timer);clearTimeout(resumeTimer);resumeTimer=null;clearWarm(true);for(const w of waiters.splice(0))w.reject(error);status('disconnected',error.message);socket?.close();if(!connecting)retry(wasConnected?0:retryDelay);}
  const ask=request=>send(frame(0x45,C.concat(v(7),text(JSON.stringify(request)))));
  async function answer(current){
   const bytes=await plain();if(current!==generation)throw Error('Disconnected. Pending actions were not resent.');
   if(bytes[4]!==0x65)throw Error('Unexpected daemon response');const r=reader(bytes.slice(5)),kind=Number(r.u());if(kind===4)throw Object.assign(Error(r.text()),{definitive:true});if(kind!==6)throw Error('Update the desktop for mobile workflows');const reply=JSON.parse(r.text());if(!reply.ok)throw Object.assign(Error(reply.error),{definitive:true});return reply.data;
  }
  function rpc(request,latest){const current=generation,mutation=!isRead(request);pendingRpc++;let submitted=false;const operation=deadline(serial.then(()=>{
   if(!connected||current!==generation)throw Error('Disconnected. The action was not resent.');
   if(latest){latest.submitted=true;request=latest.request;}ask(request);submitted=true;return answer(current);
  }),15000,'Desktop response timed out. The result may be unknown; pending actions were not resent.',current);serial=operation.catch(()=>{});return operation.catch(error=>{if(mutation&&submitted&&!error.definitive)uncertainMutation=true;throw error;}).finally(()=>pendingRpc--);}
  function rpcLatest(key,request){if(!isRead(request))return Promise.reject(Error('Only reads can be coalesced'));const previous=latestReads.get(key);if(previous&&!previous.submitted){previous.request=request;return previous.promise;}const slot={request,submitted:false};latestReads.set(key,slot);slot.promise=rpc(request,slot).finally(()=>{if(latestReads.get(key)===slot)latestReads.delete(key);});return slot.promise;}
  // 폴링은 직전 etag를 보내고, 바뀐 게 없으면 데몬이 본문을 생략한다(unchanged). 화면도 다시 그리지 않는다.
  function snapshot(current,operation){const pending=operation.then(data=>{if(current!==generation)return;if(data.unchanged&&last&&data.etag===last.etag)return last;for(const s of data.sessions||[])if(!Number.isSafeInteger(s.id)||s.id<=0)throw Error('Unsupported session ID. Update the desktop.');last=data;options.onSnapshot?.(data);return data;}).finally(()=>{if(refreshing===pending)refreshing=null;});refreshing=pending;return pending;}
  function refresh(){return refreshing||snapshot(generation,rpc({action:'snapshot',...(last?.etag&&{since:last.etag})}));}
  // 숨은 탭은 갱신하지 않고(복귀 시 resume이 즉시 갱신), 라이브 터미널이 열려 있으면 화면 스트림이 주가 되므로 간격을 늘린다.
  function poll(){clearTimeout(timer);if(!connected||stopped)return;const current=generation;timer=setTimeout(async()=>{if(!globalThis.document?.hidden)try{await refresh();}catch(e){if(connected&&current===generation)options.onError?.(e);}poll();},M.activeTerminal?10000:2500);}
  async function connect(){
   if(connecting||stopped||connected||authBlocked||options.endpoint?.updatePending)return;if(Date.now()<retryNotBefore){retry();return;}connecting=true;clearTimeout(retryTimer);clearTimeout(timer);const current=++generation,controller=connectController=new AbortController(),isCurrent=()=>!stopped&&current===generation&&!controller.signal.aborted;queue=[];waiters=[];failed=null;timing={started:clock()};status('connecting','Connecting…');await serial;if(!isCurrent())return;mark('queueMs');
   // 인증 중 전송 실패는 최대 2번 재시도하며 게이트웨이 60초 cooldown보다 오래 기다린다.
   let stage=0,attempted,quick=false;
   try{
    if(!pair&&!credentials)throw Error('Scan a fresh QR code from desktop Settings → Mobile.');
    const ensureCurrent=()=>{if(stopped||current!==generation||failed)throw failed||Error('Disconnected. Pending actions were not resent.');};
    await deadline((async()=>{
    attempted=socketUrl();const target=new URL(attempted);timing.route=target.pathname.startsWith('/r/')?'relay':target.hostname.endsWith('.trycloudflare.com')?'tunnel':'direct';const active=socket=new WebSocket(attempted);active.binaryType='arraybuffer';
    // Bound stalled opening handshakes so discovery/fallback can run.
    // Deadline includes authentication and the first snapshot.
    active.addEventListener('message',e=>{if(current!==generation)return;const w=waiters.shift();if(w)w.resolve(e.data);else queue.push(e.data);});
    const end=e=>{if(current!==generation)return;const capacity=e?.code===4009,cooldown=e?.code===1013,denied=e?.code===1008||capacity;if(denied){authBlocked=true;clearTimeout(retryTimer);}if(cooldown)retryNotBefore=Date.now()+65000;const error=Error(capacity?'Desktop device limit reached. Revoke an unused device in desktop Settings → Mobile, then scan a new QR code.':cooldown?'Authentication retry is cooling down. Wait before reconnecting.':denied?(pair?'This QR code expired or was already used. Tap New QR code on desktop and scan again.':'Desktop rejected authentication. Access may have been revoked; scan a fresh QR code.'):'Disconnected. Pending actions were not resent. If desktop access was revoked, scan a fresh QR code.');error.transport=!denied;if(denied&&failed){failed=error;status('disconnected',error.message);return;}disconnect(error);};active.addEventListener('close',end);active.addEventListener('error',end);
    await new Promise((resolve,reject)=>{active.addEventListener('open',()=>{stage=1;resolve();},{once:true});active.addEventListener('error',()=>reject(Error('Could not connect to desktop')),{once:true});active.addEventListener('close',()=>reject(failed||Error('Disconnected')),{once:true});});ensureCurrent();mark('socketOpenMs');
    const challengeBytes=await next();ensureCurrent();const challenge=JSON.parse(challengeBytes),nonce=crypto.getRandomValues(new Uint8Array(32));stage=2;mark('challengeMs');if(challenge.version!==2)throw Error('Unsupported desktop protocol');
    const pairing=!!pair,secret=pairing?C.unbase64(pair.secret):C.sha256(C.unhex(credentials.credential));channel=new C.Channel(secret,C.concat(C.unhex(challenge.challenge),nonce));secret.fill(0);
    active.send(JSON.stringify({token:pairing?pair.ticket:null,device_id:pairing?null:credentials.device_id,client_nonce:C.hex(nonce)}));send(C.enc.encode(JSON.stringify({label:options.label||'Mobile browser'})));
    // 게이트웨이는 proof 프레임만 읽고 나머지를 IPC로 넘기며 협상은 교집합이다. ServerHello를 기다리지 않고
    // ClientHello(전 비트 제시 = 서버 제시값 그대로)와 첫 snapshot 요청을 바로 보내 왕복을 줄인다.
    send(frame(2,C.concat(v(1),v(1),v(FEATURES),v(0),text('krill-mobile-pwa'))));ask({action:'snapshot',...(last?.etag&&{since:last.etag})});mark('proofSentMs');
    const authBytes=await plain();ensureCurrent();const auth=JSON.parse(decode.decode(authBytes));stage=3;mark('authenticatedMs');if(auth.credential||options.endpoint){credentials=options.endpoint?options.endpoint.authenticated(auth):auth;pair=null;options.onAuthenticated?.(credentials);}
    const hello=await plain();ensureCurrent();if(hello[4]!==1)throw Error('Unexpected daemon response');mark('helloMs');
    connected=true;warmTerminal();const first=serial.then(()=>answer(current));serial=first.catch(()=>{});const pending=snapshot(current,first),resumed=!!last;if(resumed){mark('readyMs');status('ready','Encrypted · connected');}await pending;ensureCurrent();if(!resumed)mark('readyMs');retryDelay=1000;authFailures=0;retryCount=0;nextRetryAt=0;retryNotBefore=0;warmTerminal();if(!resumed)status('ready','Encrypted · connected');poll();
    })(),20000,'Connection timed out. Pending actions were not resent.',current);if(!isCurrent())return;
   }catch(e){if(!isCurrent())return;mark('failedMs');retryCount++;if(stage<2&&!authBlocked){const located=await options.endpoint?.locate({isCurrent,signal:controller.signal});if(!isCurrent())return;if(located)stopped=true;else{try{if(socketUrl()===attempted)options.endpoint?.fallback?.(attempted);quick=retryDelay<=1000&&socketUrl()!==attempted;}catch{}}}if(stage===2){if((e?.transport||failed?.transport)&&!authBlocked){authFailures++;retryNotBefore=Date.now()+65000*authFailures;if(authFailures>2)authBlocked=true;}else authBlocked=true;}options.onError?.(e);disconnect(e);status('disconnected',failed?.message||e.message);}finally{if(connectController===controller)connectController=null;controller.abort();if(current===generation){connecting=false;if(!connected)retry(quick?0:retryDelay);}}
  }
  function reconnect(acknowledge=true){if(acknowledge)uncertainMutation=false;connectController?.abort();disconnect(Error('Reconnecting… Pending actions were not resent.'));generation++;connecting=false;refreshing=null;authBlocked=false;authFailures=0;retryDelay=1000;connect();}
  return {connect,rpc,rpcLatest,refresh,reconnect,get updateBusy(){return connecting||!!pair||pendingRpc>0||uncertainMutation;},get state(){return {...state,retryAt:Math.max(retryNotBefore,nextRetryAt),retryCount,blocked:authBlocked,timings:timings()};},setPair(nextPair){pair=nextPair;credentials=null;last=null;reconnect();},get ready(){return connected;},get blocked(){return authBlocked;},get credentials(){return credentials;},setActive(active){terminalActive=!!active;if(terminalActive)warmTerminal();else clearWarm(true);},prewarmTerminal(){warmTerminal(true);},resume(stale){if(!connected){retryDelay=1000;connect();return;}if(stale){reconnect(false);return;}if(!warm||warm.socket.readyState===2||warm.socket.readyState===3||warm.socket.closed){clearWarm(true);warmTerminal();}clearTimeout(resumeTimer);const current=generation,op=refresh();resumeTimer=setTimeout(()=>{resumeTimer=null;if(connected&&current===generation&&refreshing===op)reconnect(false);},1500);op.then(()=>{clearTimeout(resumeTimer);resumeTimer=null;},e=>{clearTimeout(resumeTimer);resumeTimer=null;if(connected&&current===generation)options.onError?.(e);});},terminalConnect(){if(!connected||!credentials)throw Error(connecting?'Connecting…':'Reconnect to desktop first');let out;if(warm&&warm.socket.readyState!==2&&warm.socket.readyState!==3&&!warm.socket.closed){const w=warm;warm=null;w.cleanup();out={socket:w.socket,credentials:w.credentials,channel:w.channel,messages:w.messages};}else{clearWarm(true);out={socket:new WebSocket(socketUrl()),credentials};}warmTerminal();return out;},close(){stopped=true;connectController?.abort();clearTimeout(timer);clearTimeout(retryTimer);clearTimeout(resumeTimer);resumeTimer=null;clearWarm(true);disconnect(Error('Disconnected'));generation++;connecting=false;}};
 }};
})();
