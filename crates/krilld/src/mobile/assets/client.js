// Workflow transport. Pure JS crypto works on LAN HTTP without SubtleCrypto.
(()=>{
 const M=globalThis.KrillMobile||= {},C=MkCrypto,decode=new TextDecoder(),FEATURES=(1n<<64n)-1n;
 const v=value=>{let n=BigInt(value),out=[];do{let b=Number(n&127n);n>>=7n;out.push(b|(n?128:0));}while(n);return Uint8Array.from(out);};
 const text=s=>{const bytes=C.enc.encode(s);return C.concat(v(bytes.length),bytes);};
 const frame=(kind,body)=>{const h=new Uint8Array(5);new DataView(h.buffer).setUint32(0,body.length+1,true);h[4]=kind;return C.concat(h,body);};
 const reader=bytes=>{let at=0;return {u(){let n=0n,shift=0n;for(let i=0;i<10;i++){if(at>=bytes.length)throw Error('Truncated response');const b=bytes[at++];n|=BigInt(b&127)<<shift;if(!(b&128))return n;shift+=7n;}throw Error('Invalid response');},text(){const n=Number(this.u());if(n>bytes.length-at)throw Error('Truncated response');const s=decode.decode(bytes.slice(at,at+n));at+=n;return s;}};};
 // Pairing input is consumed once, before UI routing or networking.
 function bootstrap(){
  const match=/^\/m\/([0-9a-f]{16})\/$/.exec(location.pathname),fixed=!!match,hash=match?.[1];
  const parameters=new URLSearchParams(location.search),fragment=new URLSearchParams(location.hash.slice(1)),ticket=fragment.get('pair'),secret=fragment.get('key'),STORE='minikrill-mobile-v2';
  let stored;try{stored=JSON.parse(localStorage.getItem(STORE)||'null');}catch{stored=null;}
  // Home Screen launches reopen the same install link (#…&install=1): it pairs only an empty store, later launches keep the saved device.
  const pair=ticket&&secret&&!(fragment.has('install')&&stored?.credential)?{ticket,secret}:null;
  const sensitive=['pair','key','t','r','install'],consumed=sensitive.some(key=>fragment.has(key));for(const key of sensitive)parameters.delete(key);
  history.replaceState({},'',location.pathname+(parameters.size?'?'+parameters:'')+(consumed?(pair?'#/pair-confirm':''):location.hash));
  let credentials=pair?null:stored;
  // A link without both pairing secrets cannot replace a saved destination.
  const rid=pair?fragment.get('r'):credentials?.rid;
  let url=pair?(fragment.get('t')?'https://'+fragment.get('t'):null):credentials?.url;
  const validUrl=value=>typeof value==='string'&&/^https:\/\/[a-z0-9-]+\.trycloudflare\.com$/.test(value),validRid=value=>typeof value==='string'&&/^[0-9a-f]{32}$/.test(value);
  let useRelay=!validUrl(url)&&validRid(rid);
  const relayUrl=()=>`${location.protocol==='https:'?'wss:':'ws:'}//${location.host}/r/${rid}?v=${hash}`;
  if(fixed)navigator.serviceWorker?.register('/sw.js').catch(()=>{});
  return {parameters,pair,credentials,fixed,
   socketUrl(){if(!fixed)return `${location.protocol==='https:'?'wss:':'ws:'}//${location.host}/ws`;if(useRelay&&validRid(rid))return relayUrl();if(validUrl(url))return `wss://${url.slice(8)}/ws?v=${hash}`;if(validRid(rid))return relayUrl();throw Error('Scan a fresh QR code from desktop Settings → Mobile.');},
   authenticated(auth){credentials=auth.credential?auth:credentials;if(fixed)credentials={...credentials,rid,url,hash};return credentials;},
   fallback(attempted){if(!fixed)return;try{if(attempted===this.socketUrl()){if(!useRelay&&validRid(rid))useRelay=true;else if(useRelay&&validUrl(url))useRelay=false;}}catch{}},
   async locate(){
    if(!fixed||!validRid(rid))return false;
    try{const response=await fetch('/r/'+encodeURIComponent(rid),{cache:'no-store'});if(response.ok){const found=await response.json();if(typeof found.hash==='string'&&/^[0-9a-f]{16}$/.test(found.hash)&&(validUrl(found.url)||found.url===''||found.relay)){
     if(validUrl(found.url)){url=found.url;useRelay=false;}else{url=null;useRelay=true;}
     if(credentials){credentials={...credentials,rid,url,hash:found.hash};localStorage.setItem(STORE,JSON.stringify(credentials));}
     if(found.hash!==hash){location.replace('/m/'+found.hash+'/'+location.search+location.hash);return true;}
    }}}catch{}
    return false;
   }
  };
 }
 M.transport={bootstrap,create(options){
  let credentials=options.credentials,pair=options.pair,socket,channel,generation=0,connected=false,connecting=false,stopped=false,failed,queue=[],waiters=[],serial=Promise.resolve(),refreshing,timer,retryTimer,resumeTimer,warm=null,retryDelay=1000,last=null,authFailures=0,retryNotBefore=0,authBlocked=false;
  const socketUrl=()=>options.endpoint?options.endpoint.socketUrl():options.url;
  const status=(phase,message)=>options.onStatus?.({phase,message,generation});
  const next=()=>queue.length?Promise.resolve(queue.shift()):failed?Promise.reject(failed):new Promise((resolve,reject)=>waiters.push({resolve,reject}));
  const send=bytes=>socket.send(channel.seal(bytes));
  const plain=async()=>channel.open(await next());
  function clearWarm(close=false){if(!warm)return;const w=warm;warm=null;w.cleanup();if(close)try{w.socket.close();}catch{}}
  // 터미널 화면은 세션·탭마다 WS를 새로 연다. 하나를 미리 인증해 두고(데몬은 모바일 첫 요청을 5분 기다린다) 꺼내 쓰면 다시 채워, 탭 전환이 연결·핸드셰이크 왕복 없이 attach 한 번이 된다.
  function warmTerminal(){if(!connected||!credentials||stopped||warm)return;let ws;try{ws=new WebSocket(socketUrl());}catch{return;}ws.binaryType='arraybuffer';const messages=[],auth=credentials,onMessage=e=>{if(warm?.socket!==ws)return;if(!warm.channel){try{const challenge=JSON.parse(e.data),nonce=crypto.getRandomValues(new Uint8Array(32));if(challenge.version!==2)throw Error('Unsupported terminal protocol');const ch=new C.Channel(C.sha256(C.unhex(auth.credential)),C.concat(C.unhex(challenge.challenge),nonce));ws.send(JSON.stringify({device_id:auth.device_id,token:null,client_nonce:C.hex(nonce)}));ws.send(ch.seal(C.enc.encode('{"label":"Live terminal"}')));ws.send(ch.seal(frame(2,C.concat(v(1),v(1),v((1n<<56n)|1n|4n|4096n),v(0),text('minikrill-mobile-grid')))));warm.channel=ch;}catch{clearWarm(true);}return;}messages.push(e.data);},onEnd=()=>{if(warm?.socket===ws)warm=null;cleanup();},cleanup=()=>{ws.removeEventListener?.('message',onMessage);ws.removeEventListener?.('close',onEnd);ws.removeEventListener?.('error',onEnd);};ws.addEventListener('message',onMessage);ws.addEventListener('close',onEnd);ws.addEventListener('error',onEnd);warm={socket:ws,credentials:auth,channel:null,messages,cleanup};}
  function retry(delay=retryDelay){clearTimeout(retryTimer);if(stopped||authBlocked||(!credentials&&!pair))return;retryTimer=setTimeout(connect,Math.max(delay,retryNotBefore-Date.now()));retryDelay=Math.min(retryDelay*2,15000);}
  function disconnect(error){if(failed)return;failed=error;connected=false;clearTimeout(timer);clearTimeout(resumeTimer);resumeTimer=null;clearWarm(true);for(const w of waiters.splice(0))w.reject(error);status('disconnected',error.message);socket?.close();if(!connecting)retry();}
  const ask=request=>send(frame(0x45,C.concat(v(7),text(JSON.stringify(request)))));
  async function answer(current){
   const bytes=await plain();if(current!==generation)throw Error('Disconnected. Pending actions were not resent.');
   if(bytes[4]!==0x65)throw Error('Unexpected daemon response');const r=reader(bytes.slice(5)),kind=Number(r.u());if(kind===4)throw Error(r.text());if(kind!==6)throw Error('Update the desktop for mobile workflows');const reply=JSON.parse(r.text());if(!reply.ok)throw Error(reply.error);return reply.data;
  }
  function rpc(request){const current=generation;const operation=serial.then(()=>{
   if(!connected||current!==generation)throw Error('Disconnected. The action was not resent.');
   ask(request);return answer(current);
  });serial=operation.catch(()=>{});return operation;}
  // 폴링은 직전 etag를 보내고, 바뀐 게 없으면 데몬이 본문을 생략한다(unchanged). 화면도 다시 그리지 않는다.
  function snapshot(current,operation){refreshing=operation.then(data=>{if(current!==generation)return;if(data.unchanged&&last&&data.etag===last.etag)return last;for(const s of data.sessions||[])if(!Number.isSafeInteger(s.id)||s.id<=0)throw Error('Unsupported session ID. Update the desktop.');last=data;options.onSnapshot?.(data);return data;}).finally(()=>refreshing=null);return refreshing;}
  function refresh(){return refreshing||snapshot(generation,rpc({action:'snapshot',...(last?.etag&&{since:last.etag})}));}
  // 숨은 탭은 갱신하지 않고(복귀 시 resume이 즉시 갱신), 라이브 터미널이 열려 있으면 화면 스트림이 주가 되므로 간격을 늘린다.
  function poll(){clearTimeout(timer);if(!connected||stopped)return;const current=generation;timer=setTimeout(async()=>{if(!globalThis.document?.hidden)try{await refresh();}catch(e){if(connected&&current===generation)options.onError?.(e);}poll();},M.activeTerminal?10000:2500);}
  async function connect(){
   if(connecting||stopped||connected||authBlocked)return;if(Date.now()<retryNotBefore){retry();return;}connecting=true;clearTimeout(retryTimer);clearTimeout(timer);const current=++generation;queue=[];waiters=[];failed=null;status('connecting','Connecting…');await serial;if(stopped||current!==generation)return;
   // 인증 중 전송 실패는 최대 2번 재시도하며 게이트웨이 60초 cooldown보다 오래 기다린다.
   let stage=0,attempted;
   try{
    if(!pair&&!credentials)throw Error('Scan a fresh QR code from desktop Settings → Mobile.');
    attempted=socketUrl();const active=socket=new WebSocket(attempted);active.binaryType='arraybuffer';
    active.addEventListener('message',e=>{if(current!==generation)return;const w=waiters.shift();if(w)w.resolve(e.data);else queue.push(e.data);});
    const end=e=>{if(current!==generation)return;const denied=e?.code===1008;if(denied){authBlocked=true;clearTimeout(retryTimer);}const error=Error(denied?'Desktop rejected authentication. Scan a fresh QR code.':'Disconnected. Pending actions were not resent.');error.transport=!denied;if(denied&&failed){failed=error;status('disconnected',error.message);return;}disconnect(error);};active.addEventListener('close',end);active.addEventListener('error',end);
    await new Promise((resolve,reject)=>{active.addEventListener('open',()=>{stage=1;resolve();},{once:true});active.addEventListener('error',()=>reject(Error('Could not connect to desktop')),{once:true});active.addEventListener('close',()=>reject(failed||Error('Disconnected')),{once:true});});
    const challenge=JSON.parse(await next()),nonce=crypto.getRandomValues(new Uint8Array(32));stage=2;if(challenge.version!==2)throw Error('Unsupported desktop protocol');
    const pairing=!!pair,secret=pairing?C.unbase64(pair.secret):C.sha256(C.unhex(credentials.credential));channel=new C.Channel(secret,C.concat(C.unhex(challenge.challenge),nonce));secret.fill(0);
    active.send(JSON.stringify({token:pairing?pair.ticket:null,device_id:pairing?null:credentials.device_id,client_nonce:C.hex(nonce)}));send(C.enc.encode(JSON.stringify({label:options.label||'Mobile browser'})));
    // 게이트웨이는 proof 프레임만 읽고 나머지를 IPC로 넘기며 협상은 교집합이다. ServerHello를 기다리지 않고
    // ClientHello(전 비트 제시 = 서버 제시값 그대로)와 첫 snapshot 요청을 바로 보내 왕복을 줄인다.
    send(frame(2,C.concat(v(1),v(1),v(FEATURES),v(0),text('krill-mobile-pwa'))));ask({action:'snapshot'});
    const auth=JSON.parse(decode.decode(await plain()));stage=3;if(auth.credential||options.endpoint?.fixed){credentials=options.endpoint?options.endpoint.authenticated(auth):auth;pair=null;options.onAuthenticated?.(credentials);}
    if((await plain())[4]!==1)throw Error('Unexpected daemon response');
    connected=true;const first=serial.then(()=>answer(current));serial=first.catch(()=>{});await snapshot(current,first);retryDelay=1000;authFailures=0;retryNotBefore=0;warmTerminal();status('ready','Encrypted · connected');poll();
   }catch(e){if(stage===0&&!authBlocked){if(await options.endpoint?.locate())stopped=true;else options.endpoint?.fallback?.(attempted);}if(stage===2){if((e?.transport||failed?.transport)&&!authBlocked){authFailures++;retryNotBefore=Date.now()+65000*authFailures;if(authFailures>2)authBlocked=true;}else authBlocked=true;}options.onError?.(e);disconnect(e);}finally{connecting=false;if(!connected)retry();}
  }
  function reconnect(){disconnect(Error('Reconnecting… Pending actions were not resent.'));authBlocked=false;authFailures=0;retryNotBefore=0;retryDelay=1000;connect();}
  return {connect,rpc,refresh,reconnect,get ready(){return connected;},get blocked(){return authBlocked;},get credentials(){return credentials;},resume(){if(!connected){retryDelay=1000;connect();return;}if(!warm||warm.socket.readyState===2||warm.socket.readyState===3||warm.socket.closed){clearWarm(true);warmTerminal();}clearTimeout(resumeTimer);const current=generation,op=refresh();resumeTimer=setTimeout(()=>{resumeTimer=null;if(connected&&current===generation&&refreshing===op)reconnect();},1500);op.then(()=>{clearTimeout(resumeTimer);resumeTimer=null;},e=>{clearTimeout(resumeTimer);resumeTimer=null;if(connected&&current===generation)options.onError?.(e);});},terminalConnect(){if(!connected||!credentials)throw Error(connecting?'Connecting…':'Reconnect to desktop first');let out;if(warm&&warm.socket.readyState!==2&&warm.socket.readyState!==3&&!warm.socket.closed){const w=warm;warm=null;w.cleanup();out={socket:w.socket,credentials:w.credentials,channel:w.channel,messages:w.messages};}else{clearWarm(true);out={socket:new WebSocket(socketUrl()),credentials};}warmTerminal();return out;},close(){stopped=true;clearTimeout(timer);clearTimeout(retryTimer);clearTimeout(resumeTimer);resumeTimer=null;clearWarm(true);disconnect(Error('Disconnected'));}};
 }};
})();
