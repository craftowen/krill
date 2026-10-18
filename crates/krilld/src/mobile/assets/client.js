// Workflow transport. Pure JS crypto works on LAN HTTP without SubtleCrypto.
(()=>{
 const M=globalThis.KrillMobile||= {},C=MkCrypto,decode=new TextDecoder();
 const v=value=>{let n=BigInt(value),out=[];do{let b=Number(n&127n);n>>=7n;out.push(b|(n?128:0));}while(n);return Uint8Array.from(out);};
 const text=s=>{const bytes=C.enc.encode(s);return C.concat(v(bytes.length),bytes);};
 const frame=(kind,body)=>{const h=new Uint8Array(5);new DataView(h.buffer).setUint32(0,body.length+1,true);h[4]=kind;return C.concat(h,body);};
 const reader=bytes=>{let at=0;return {u(){let n=0n,shift=0n;for(let i=0;i<10;i++){if(at>=bytes.length)throw Error('Truncated response');const b=bytes[at++];n|=BigInt(b&127)<<shift;if(!(b&128))return n;shift+=7n;}throw Error('Invalid response');},text(){const n=Number(this.u());if(n>bytes.length-at)throw Error('Truncated response');const s=decode.decode(bytes.slice(at,at+n));at+=n;return s;}};};
 M.transport={create(options){
  let credentials=options.credentials,pair=options.pair,socket,channel,generation=0,connected=false,connecting=false,stopped=false,failed,queue=[],waiters=[],serial=Promise.resolve(),refreshing,timer,retryTimer,retryDelay=1000,authFailures=0,retryNotBefore=0,authBlocked=false;
  const status=(phase,message)=>options.onStatus?.({phase,message,generation});
  const next=()=>queue.length?Promise.resolve(queue.shift()):failed?Promise.reject(failed):new Promise((resolve,reject)=>waiters.push({resolve,reject}));
  const send=bytes=>socket.send(channel.seal(bytes));
  const plain=async()=>channel.open(await next());
  function retry(delay=retryDelay){clearTimeout(retryTimer);if(stopped||authBlocked||!credentials)return;retryTimer=setTimeout(connect,Math.max(delay,retryNotBefore-Date.now()));retryDelay=Math.min(retryDelay*2,15000);}
  function disconnect(error){if(failed)return;failed=error;connected=false;clearTimeout(timer);for(const w of waiters.splice(0))w.reject(error);status('disconnected',error.message);socket?.close();if(!connecting)retry();}
  function rpc(request){const current=generation;const operation=serial.then(async()=>{
   if(!connected||current!==generation)throw Error('Disconnected. The action was not resent.');
   send(frame(0x45,C.concat(v(7),text(JSON.stringify(request)))));
   const bytes=await plain();if(current!==generation)throw Error('Disconnected. Pending actions were not resent.');
   if(bytes[4]!==0x65)throw Error('Unexpected daemon response');const r=reader(bytes.slice(5)),kind=Number(r.u());if(kind===4)throw Error(r.text());if(kind!==6)throw Error('Update the desktop for mobile workflows');const reply=JSON.parse(r.text());if(!reply.ok)throw Error(reply.error);return reply.data;
  });serial=operation.catch(()=>{});return operation;}
  function refresh(){if(refreshing)return refreshing;const current=generation;refreshing=rpc({action:'snapshot'}).then(data=>{if(current!==generation)return;for(const s of data.sessions||[])if(!Number.isSafeInteger(s.id)||s.id<=0)throw Error('Unsupported session ID. Update the desktop.');options.onSnapshot?.(data);return data;}).finally(()=>refreshing=null);return refreshing;}
  function poll(){clearTimeout(timer);if(!connected||stopped)return;timer=setTimeout(async()=>{try{await refresh();}catch(e){options.onError?.(e);}poll();},2500);}
  async function connect(){
   if(connecting||stopped||connected||authBlocked)return;if(Date.now()<retryNotBefore){retry();return;}connecting=true;clearTimeout(retryTimer);clearTimeout(timer);await serial;const current=++generation;queue=[];waiters=[];failed=null;status('connecting','Connecting…');
   // 인증 중 전송 실패는 최대 2번 재시도하며 게이트웨이 60초 cooldown보다 오래 기다린다.
   let stage=0;
   try{
    if(!pair&&!credentials)throw Error('Scan a fresh QR code from desktop Settings → Mobile.');
    const active=socket=new WebSocket(options.url);active.binaryType='arraybuffer';
    active.addEventListener('message',e=>{if(current!==generation)return;const w=waiters.shift();if(w)w.resolve(e.data);else queue.push(e.data);});
    const end=e=>{if(current!==generation)return;const denied=e?.code===1008;if(denied){authBlocked=true;clearTimeout(retryTimer);}const error=Error(denied?'Desktop rejected authentication. Scan a fresh QR code.':'Disconnected. Pending actions were not resent.');error.transport=!denied;if(denied&&failed){failed=error;status('disconnected',error.message);return;}disconnect(error);};active.addEventListener('close',end);active.addEventListener('error',end);
    await new Promise((resolve,reject)=>{active.addEventListener('open',resolve,{once:true});active.addEventListener('error',()=>reject(Error('Could not connect to desktop')),{once:true});active.addEventListener('close',()=>reject(failed||Error('Disconnected')),{once:true});});
    const challenge=JSON.parse(await next()),nonce=crypto.getRandomValues(new Uint8Array(32));stage=2;if(challenge.version!==2)throw Error('Unsupported desktop protocol');
    const pairing=!!pair,secret=pairing?C.unbase64(pair.secret):C.sha256(C.unhex(credentials.credential));channel=new C.Channel(secret,C.concat(C.unhex(challenge.challenge),nonce));secret.fill(0);
    active.send(JSON.stringify({token:pairing?pair.ticket:null,device_id:pairing?null:credentials.device_id,client_nonce:C.hex(nonce)}));send(C.enc.encode(JSON.stringify({label:options.label||'Mobile browser'})));
    const auth=JSON.parse(decode.decode(await plain()));stage=3;if(auth.credential){credentials=auth;pair=null;options.onAuthenticated?.(auth);}
    const hello=reader((await plain()).slice(5));hello.u();hello.u();const features=hello.u();send(frame(2,C.concat(v(1),v(1),v(features),v(0),text('krill-mobile-pwa'))));
    connected=true;await refresh();retryDelay=1000;authFailures=0;retryNotBefore=0;status('ready','Encrypted · connected');poll();
   }catch(e){if(stage===2){if((e?.transport||failed?.transport)&&!authBlocked){authFailures++;retryNotBefore=Date.now()+65000*authFailures;if(authFailures>2)authBlocked=true;}else authBlocked=true;}options.onError?.(e);disconnect(e);}finally{connecting=false;if(!connected)retry();}
  }
  return {connect,rpc,refresh,reconnect(){disconnect(Error('Reconnecting… Pending actions were not resent.'));authBlocked=false;authFailures=0;retryNotBefore=0;retryDelay=1000;connect();},get ready(){return connected;},get credentials(){return credentials;},resume(){if(!connected){retryDelay=1000;connect();}else refresh().catch(options.onError);},terminalConnect(){if(!connected||!credentials)throw Error('Reconnect to desktop first');return {socket:new WebSocket(options.url),credentials};},close(){stopped=true;clearTimeout(timer);clearTimeout(retryTimer);disconnect(Error('Disconnected'));}};
 }};
})();
