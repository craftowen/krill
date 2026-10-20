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
  const parameters=new URLSearchParams(location.search),fragment=new URLSearchParams(location.hash.slice(1)),ticket=fragment.get('pair'),secret=fragment.get('key'),pair=ticket&&secret?{ticket,secret}:null;
  const auth=parameters.get('auth'),sensitive=['pair','key','t','r'],consumed=sensitive.some(key=>fragment.has(key));for(const key of [...sensitive,'auth'])parameters.delete(key);
  history.replaceState({},'',location.pathname+(parameters.size?'?'+parameters:'')+(consumed?(pair?'#/pair-confirm':''):location.hash));
  const STORE='minikrill-mobile-v2',HOSTS='krill-mobile-hosts-v1';
  let saved,book;try{saved=JSON.parse(localStorage.getItem(STORE)||'null');}catch{saved=null;}
  try{book=JSON.parse(localStorage.getItem(HOSTS)||'null');}catch{}const hosts=Array.isArray(book?.hosts)?book.hosts.filter(h=>typeof h?.id==='string'&&typeof h.device_id==='string'&&typeof h.credential==='string'):[];
  let active=hosts.find(h=>h.id===book?.active)||null;if(active)saved=active;let credentials=pair?null:saved,directOrigin=location.origin,destinationFixed=fixed;
  const hostId=c=>c.rid?'relay:'+c.rid:c.url?'tunnel:'+c.url:'direct:'+(c.origin||location.origin);
  let storageFailed=false;const persist=()=>{try{localStorage.setItem(HOSTS,JSON.stringify({active:active?.id||null,hosts}));storageFailed=false;}catch{storageFailed=true;}};
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
  let useRelay=!validUrl(url)&&validRid(rid);
  const relayUrl=()=>`${location.protocol==='https:'?'wss:':'ws:'}//${location.host}/r/${rid}?v=${hash}`;
  navigator.serviceWorker?.register('/sw.js').catch(()=>{});
  return {parameters,pair,credentials,fixed,replacing:!!(pair&&saved),get storageFailed(){return storageFailed;},
   profiles(){return hosts.map(h=>({id:h.id,name:h.name||'Desktop',platform:h.platform,endpoint:h.fixed?(h.rid?'Relay · '+h.rid.slice(0,8):h.url):h.origin,active:h.id===active?.id}));},
   selectHost(id){const host=hosts.find(h=>h.id===id);if(!host)throw Error('Desktop is no longer saved.');active=host;saved=credentials=host;rid=host.rid;url=host.url;directOrigin=host.origin||location.origin;destinationFixed=host.fixed??fixed;useRelay=!validUrl(url)&&validRid(rid);persist();try{localStorage.setItem(STORE,JSON.stringify(host));}catch{storageFailed=true;}return credentials;},
   forgetHost(id){const at=hosts.findIndex(h=>h.id===id);if(at<0)return;if(active?.id===id)throw Error('Switch to another desktop before removing this one.');hosts.splice(at,1);persist();},
   describeHost(host){if(!active||!credentials)return;let changed=false;for(const [key,value] of [['name',host?.display_name],['platform',host?.platform]])if(value&&active[key]!==value){active[key]=value;changed=true;}if(changed)persist();},
   restoreSaved(){credentials=saved;directOrigin=active?.origin||location.origin;destinationFixed=active?.fixed??fixed;rid=saved?.rid;url=saved?.url;useRelay=!validUrl(url)&&validRid(rid);if(fixed&&/^[0-9a-f]{16}$/.test(saved?.hash||'')&&saved.hash!==hash)location.replace('/m/'+saved.hash+'/');return credentials;},
   socketUrl(){if(!destinationFixed){const origin=new URL(directOrigin);return `${origin.protocol==='https:'?'wss:':'ws:'}//${origin.host}/ws`;}if(useRelay&&validRid(rid))return relayUrl();if(validUrl(url))return `wss://${url.slice(8)}/ws?v=${hash}`;if(validRid(rid))return relayUrl();throw Error('Scan a fresh QR code from desktop Settings → Mobile.');},
   // use() adopts reachable codes; null opens another origin or app version.
   adopt(href){
    const NOT='Not a Krill pairing code. Scan the QR code in desktop Settings → Mobile.';let u;try{u=new URL(href);}catch{throw Error(NOT);}
    const f=new URLSearchParams(u.hash.slice(1)),ticket=f.get('pair'),secret=f.get('key'),version=/^\/m\/([0-9a-f]{16})\/?$/.exec(u.pathname)?.[1];
    if(!/^[0-9a-f]+$/.test(ticket||'')||!secret||!/^https?:$/.test(u.protocol))throw Error(NOT);
    if(!fixed)return u.origin===location.origin?{ticket,secret,use(){destinationFixed=false;directOrigin=u.origin;}}:null;
    if(u.origin===location.origin&&version&&version!==hash)return null;
    const t=f.get('t'),r=f.get('r'),next=t?'https://'+t:validUrl(u.origin)?u.origin:null;
    if(!validUrl(next)&&!validRid(r))throw Error(u.protocol==='http:'?'This direct code cannot be opened here. Choose Relay server in desktop Settings → Mobile, then tap New QR code.':NOT);
    return {ticket,secret,use(){destinationFixed=true;url=validUrl(next)?next:null;rid=validRid(r)?r:null;useRelay=!url&&!!rid;}};
   },
   authenticated(auth){credentials=auth.credential?auth:credentials;if(destinationFixed)credentials={...credentials,rid,url,hash};
    if(!credentials)return null;const id=hostId({...credentials,origin:directOrigin}),existing=hosts.find(h=>h.id===id);active={...existing,...credentials,id,origin:directOrigin,fixed:destinationFixed,name:existing?.name||'Desktop'};if(existing)hosts[hosts.indexOf(existing)]=active;else hosts.push(active);saved=credentials=active;persist();try{localStorage.setItem(STORE,JSON.stringify(credentials));}catch{storageFailed=true;}return credentials;},
   fallback(attempted){if(!destinationFixed)return;try{if(attempted===this.socketUrl()){if(!useRelay&&validRid(rid))useRelay=true;else if(useRelay&&validUrl(url))useRelay=false;}}catch{}},
   // Cancelled discovery cannot replace a newer destination.
   async locate({isCurrent=()=>true,signal}={}){
    if(!destinationFixed||!validRid(rid))return false;
    const requestRid=rid,controller=new AbortController(),abort=()=>controller.abort(),current=()=>!controller.signal.aborted&&!signal?.aborted&&isCurrent()&&rid===requestRid;
    if(!current())return false;signal?.addEventListener('abort',abort,{once:true});const timeout=setTimeout(abort,5000);
    try{const response=await fetch('/r/'+encodeURIComponent(requestRid),{cache:'no-store',signal:controller.signal});if(!current())return false;if(response.ok){const found=await response.json();if(!current())return false;if(typeof found.hash==='string'&&/^[0-9a-f]{16}$/.test(found.hash)&&(validUrl(found.url)||found.url===''||found.relay)){
     if(validUrl(found.url)){url=found.url;useRelay=false;}else{url=null;useRelay=true;}
     if(credentials){credentials={...credentials,rid,url,hash:found.hash};if(active){Object.assign(active,credentials);saved=active;persist();}localStorage.setItem(STORE,JSON.stringify(credentials));}
     if(found.hash!==hash){location.replace('/m/'+found.hash+'/'+location.search+location.hash);return true;}
    }}}catch{}finally{clearTimeout(timeout);signal?.removeEventListener('abort',abort);}
    return false;
   }
  };
 }
 M.transport={bootstrap,handoff,create(options){
  let credentials=options.credentials,pair=options.pair,socket,channel,generation=0,connected=false,connecting=false,stopped=false,failed,queue=[],waiters=[],serial=Promise.resolve(),refreshing,timer,retryTimer,resumeTimer,connectController,warm=null,retryDelay=1000,last=null,authFailures=0,retryNotBefore=0,authBlocked=false,retryCount=0,nextRetryAt=0;
  const socketUrl=()=>options.endpoint?options.endpoint.socketUrl():options.url;
  let state={phase:'disconnected',message:'',generation,retryAt:0,blocked:false};
  const status=(phase,message)=>{state={phase,message,generation,retryAt:Math.max(retryNotBefore,nextRetryAt),retryCount,blocked:authBlocked};options.onStatus?.(state);};
  function deadline(operation,ms,message,current=generation){let timeout;return Promise.race([operation,new Promise((_,reject)=>{timeout=setTimeout(()=>{const error=Error(message);error.transport=true;reject(error);if(current===generation)disconnect(error);},ms);})]).finally(()=>clearTimeout(timeout));}
  const next=()=>queue.length?Promise.resolve(queue.shift()):failed?Promise.reject(failed):new Promise((resolve,reject)=>waiters.push({resolve,reject}));
  const send=bytes=>socket.send(channel.seal(bytes));
  const plain=async()=>channel.open(await next());
  function clearWarm(close=false){if(!warm)return;const w=warm;warm=null;w.cleanup();if(close)try{w.socket.close();}catch{}}
  // 미리 인증한 WS 하나를 터미널 탭 전환에 재사용한다. 첫 IPC 요청 대기는 5분.
  function warmTerminal(){if(!connected||!credentials||stopped||warm)return;let ws;try{ws=new WebSocket(socketUrl());}catch{return;}ws.binaryType='arraybuffer';const messages=[],auth=credentials,onMessage=e=>{if(warm?.socket!==ws)return;if(!warm.channel){try{const challenge=JSON.parse(e.data),nonce=crypto.getRandomValues(new Uint8Array(32));if(challenge.version!==2)throw Error('Unsupported terminal protocol');const ch=new C.Channel(C.sha256(C.unhex(auth.credential)),C.concat(C.unhex(challenge.challenge),nonce));ws.send(JSON.stringify({device_id:auth.device_id,token:null,client_nonce:C.hex(nonce)}));ws.send(ch.seal(C.enc.encode('{"label":"Live terminal"}')));ws.send(ch.seal(frame(2,C.concat(v(1),v(1),v((1n<<56n)|1n|4n|4096n|8192n),v(0),text('minikrill-mobile-grid')))));warm.channel=ch;}catch{clearWarm(true);}return;}messages.push(e.data);},onEnd=()=>{if(warm?.socket===ws)warm=null;cleanup();},cleanup=()=>{ws.removeEventListener?.('message',onMessage);ws.removeEventListener?.('close',onEnd);ws.removeEventListener?.('error',onEnd);};ws.addEventListener('message',onMessage);ws.addEventListener('close',onEnd);ws.addEventListener('error',onEnd);warm={socket:ws,credentials:auth,channel:null,messages,cleanup};}
  function retry(delay=retryDelay){clearTimeout(retryTimer);if(stopped||authBlocked||(!credentials&&!pair))return;const wait=Math.max(delay,retryNotBefore-Date.now());nextRetryAt=Date.now()+wait;status('disconnected',retryNotBefore>Date.now()?'Authentication retry is cooling down. Wait, or scan a fresh QR code.':failed?.message||'Reconnecting…');retryTimer=setTimeout(connect,wait);retryDelay=Math.min(retryDelay*2,15000);}
  function disconnect(error){if(failed)return;failed=error;connected=false;clearTimeout(timer);clearTimeout(resumeTimer);resumeTimer=null;clearWarm(true);for(const w of waiters.splice(0))w.reject(error);status('disconnected',error.message);socket?.close();if(!connecting)retry();}
  const ask=request=>send(frame(0x45,C.concat(v(7),text(JSON.stringify(request)))));
  async function answer(current){
   const bytes=await plain();if(current!==generation)throw Error('Disconnected. Pending actions were not resent.');
   if(bytes[4]!==0x65)throw Error('Unexpected daemon response');const r=reader(bytes.slice(5)),kind=Number(r.u());if(kind===4)throw Error(r.text());if(kind!==6)throw Error('Update the desktop for mobile workflows');const reply=JSON.parse(r.text());if(!reply.ok)throw Error(reply.error);return reply.data;
  }
  function rpc(request){const current=generation;const operation=deadline(serial.then(()=>{
   if(!connected||current!==generation)throw Error('Disconnected. The action was not resent.');
   ask(request);return answer(current);
  }),15000,'Desktop response timed out. The result may be unknown; pending actions were not resent.',current);serial=operation.catch(()=>{});return operation;}
  // 폴링은 직전 etag를 보내고, 바뀐 게 없으면 데몬이 본문을 생략한다(unchanged). 화면도 다시 그리지 않는다.
  function snapshot(current,operation){const pending=operation.then(data=>{if(current!==generation)return;if(data.unchanged&&last&&data.etag===last.etag)return last;for(const s of data.sessions||[])if(!Number.isSafeInteger(s.id)||s.id<=0)throw Error('Unsupported session ID. Update the desktop.');last=data;options.onSnapshot?.(data);return data;}).finally(()=>{if(refreshing===pending)refreshing=null;});refreshing=pending;return pending;}
  function refresh(){return refreshing||snapshot(generation,rpc({action:'snapshot',...(last?.etag&&{since:last.etag})}));}
  // 숨은 탭은 갱신하지 않고(복귀 시 resume이 즉시 갱신), 라이브 터미널이 열려 있으면 화면 스트림이 주가 되므로 간격을 늘린다.
  function poll(){clearTimeout(timer);if(!connected||stopped)return;const current=generation;timer=setTimeout(async()=>{if(!globalThis.document?.hidden)try{await refresh();}catch(e){if(connected&&current===generation)options.onError?.(e);}poll();},M.activeTerminal?10000:2500);}
  async function connect(){
   if(connecting||stopped||connected||authBlocked)return;if(Date.now()<retryNotBefore){retry();return;}connecting=true;clearTimeout(retryTimer);clearTimeout(timer);const current=++generation,controller=connectController=new AbortController(),isCurrent=()=>!stopped&&current===generation&&!controller.signal.aborted;queue=[];waiters=[];failed=null;status('connecting','Connecting…');await serial;if(!isCurrent())return;
   // 인증 중 전송 실패는 최대 2번 재시도하며 게이트웨이 60초 cooldown보다 오래 기다린다.
   let stage=0,attempted,quick=false;
   try{
    if(!pair&&!credentials)throw Error('Scan a fresh QR code from desktop Settings → Mobile.');
    const ensureCurrent=()=>{if(stopped||current!==generation||failed)throw failed||Error('Disconnected. Pending actions were not resent.');};
    await deadline((async()=>{
    attempted=socketUrl();const active=socket=new WebSocket(attempted);active.binaryType='arraybuffer';
    // Bound stalled opening handshakes so discovery/fallback can run.
    // Deadline includes authentication and the first snapshot.
    active.addEventListener('message',e=>{if(current!==generation)return;const w=waiters.shift();if(w)w.resolve(e.data);else queue.push(e.data);});
    const end=e=>{if(current!==generation)return;const capacity=e?.code===4009,cooldown=e?.code===1013,denied=e?.code===1008||capacity;if(denied){authBlocked=true;clearTimeout(retryTimer);}if(cooldown)retryNotBefore=Date.now()+65000;const error=Error(capacity?'Desktop device limit reached. Revoke an unused device in desktop Settings → Mobile, then scan a new QR code.':cooldown?'Authentication retry is cooling down. Wait before reconnecting.':denied?(pair?'This QR code expired or was already used. Tap New QR code on desktop and scan again.':'Desktop rejected authentication. Access may have been revoked; scan a fresh QR code.'):'Disconnected. Pending actions were not resent. If desktop access was revoked, scan a fresh QR code.');error.transport=!denied;if(denied&&failed){failed=error;status('disconnected',error.message);return;}disconnect(error);};active.addEventListener('close',end);active.addEventListener('error',end);
    await new Promise((resolve,reject)=>{active.addEventListener('open',()=>{stage=1;resolve();},{once:true});active.addEventListener('error',()=>reject(Error('Could not connect to desktop')),{once:true});active.addEventListener('close',()=>reject(failed||Error('Disconnected')),{once:true});});ensureCurrent();
    const challengeBytes=await next();ensureCurrent();const challenge=JSON.parse(challengeBytes),nonce=crypto.getRandomValues(new Uint8Array(32));stage=2;if(challenge.version!==2)throw Error('Unsupported desktop protocol');
    const pairing=!!pair,secret=pairing?C.unbase64(pair.secret):C.sha256(C.unhex(credentials.credential));channel=new C.Channel(secret,C.concat(C.unhex(challenge.challenge),nonce));secret.fill(0);
    active.send(JSON.stringify({token:pairing?pair.ticket:null,device_id:pairing?null:credentials.device_id,client_nonce:C.hex(nonce)}));send(C.enc.encode(JSON.stringify({label:options.label||'Mobile browser'})));
    // 게이트웨이는 proof 프레임만 읽고 나머지를 IPC로 넘기며 협상은 교집합이다. ServerHello를 기다리지 않고
    // ClientHello(전 비트 제시 = 서버 제시값 그대로)와 첫 snapshot 요청을 바로 보내 왕복을 줄인다.
    send(frame(2,C.concat(v(1),v(1),v(FEATURES),v(0),text('krill-mobile-pwa'))));ask({action:'snapshot'});
    const authBytes=await plain();ensureCurrent();const auth=JSON.parse(decode.decode(authBytes));stage=3;if(auth.credential||options.endpoint){credentials=options.endpoint?options.endpoint.authenticated(auth):auth;pair=null;options.onAuthenticated?.(credentials);}
    const hello=await plain();ensureCurrent();if(hello[4]!==1)throw Error('Unexpected daemon response');
    connected=true;const first=serial.then(()=>answer(current));serial=first.catch(()=>{});await snapshot(current,first);ensureCurrent();retryDelay=1000;authFailures=0;retryCount=0;nextRetryAt=0;retryNotBefore=0;warmTerminal();status('ready','Encrypted · connected');poll();
    })(),20000,'Connection timed out. Pending actions were not resent.',current);if(!isCurrent())return;
   }catch(e){if(!isCurrent())return;retryCount++;if(stage===0&&!authBlocked){const located=await options.endpoint?.locate({isCurrent,signal:controller.signal});if(!isCurrent())return;if(located)stopped=true;else{options.endpoint?.fallback?.(attempted);try{quick=retryDelay<=1000&&socketUrl()!==attempted;}catch{}}}if(stage===2){if((e?.transport||failed?.transport)&&!authBlocked){authFailures++;retryNotBefore=Date.now()+65000*authFailures;if(authFailures>2)authBlocked=true;}else authBlocked=true;}options.onError?.(e);disconnect(e);status('disconnected',failed?.message||e.message);}finally{if(connectController===controller)connectController=null;controller.abort();if(current===generation){connecting=false;if(!connected)retry(quick?0:retryDelay);}}
  }
  function reconnect(){connectController?.abort();disconnect(Error('Reconnecting… Pending actions were not resent.'));generation++;connecting=false;refreshing=null;authBlocked=false;authFailures=0;retryDelay=1000;connect();}
  return {connect,rpc,refresh,reconnect,get state(){return {...state,retryAt:Math.max(retryNotBefore,nextRetryAt),retryCount,blocked:authBlocked};},setPair(nextPair){pair=nextPair;credentials=null;reconnect();},get ready(){return connected;},get blocked(){return authBlocked;},get credentials(){return credentials;},resume(stale){if(!connected){retryDelay=1000;connect();return;}if(stale){reconnect();return;}if(!warm||warm.socket.readyState===2||warm.socket.readyState===3||warm.socket.closed){clearWarm(true);warmTerminal();}clearTimeout(resumeTimer);const current=generation,op=refresh();resumeTimer=setTimeout(()=>{resumeTimer=null;if(connected&&current===generation&&refreshing===op)reconnect();},1500);op.then(()=>{clearTimeout(resumeTimer);resumeTimer=null;},e=>{clearTimeout(resumeTimer);resumeTimer=null;if(connected&&current===generation)options.onError?.(e);});},terminalConnect(){if(!connected||!credentials)throw Error(connecting?'Connecting…':'Reconnect to desktop first');let out;if(warm&&warm.socket.readyState!==2&&warm.socket.readyState!==3&&!warm.socket.closed){const w=warm;warm=null;w.cleanup();out={socket:w.socket,credentials:w.credentials,channel:w.channel,messages:w.messages};}else{clearWarm(true);out={socket:new WebSocket(socketUrl()),credentials};}warmTerminal();return out;},close(){stopped=true;connectController?.abort();clearTimeout(timer);clearTimeout(retryTimer);clearTimeout(resumeTimer);resumeTimer=null;clearWarm(true);disconnect(Error('Disconnected'));generation++;connecting=false;}};
 }};
})();
