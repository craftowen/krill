/* M9 E2EE v2. Offline implementation: SHA-256, HKDF, ChaCha20, Poly1305. */
'use strict';
const MkCrypto=(()=>{
 const enc=new TextEncoder();
 const concat=(...parts)=>{const b=new Uint8Array(parts.reduce((n,p)=>n+p.length,0));let at=0;for(const p of parts){b.set(p,at);at+=p.length;}return b;};
 const rotr=(v,n)=>(v>>>n)|(v<<(32-n));
 const K=Uint32Array.from([0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2]);
 function sha256(input){
  const n=Math.ceil((input.length+9)/64)*64,b=new Uint8Array(n);b.set(input);b[input.length]=128;
  let length=BigInt(input.length)*8n;for(let i=0;i<8;i++){b[n-1-i]=Number(length&255n);length>>=8n;}
  const h=Uint32Array.from([0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19]),w=new Uint32Array(64);
  for(let at=0;at<n;at+=64){
   for(let i=0;i<16;i++){let p=at+i*4;w[i]=((b[p]<<24)|(b[p+1]<<16)|(b[p+2]<<8)|b[p+3])>>>0;}
   for(let i=16;i<64;i++){const a=w[i-15],c=w[i-2],s0=rotr(a,7)^rotr(a,18)^(a>>>3),s1=rotr(c,17)^rotr(c,19)^(c>>>10);w[i]=(w[i-16]+s0+w[i-7]+s1)>>>0;}
   let [a,c,d,e,f,g,j,k]=h;
   for(let i=0;i<64;i++){const s1=rotr(f,6)^rotr(f,11)^rotr(f,25),ch=(f&g)^(~f&j),t1=(k+s1+ch+K[i]+w[i])>>>0,s0=rotr(a,2)^rotr(a,13)^rotr(a,22),maj=(a&c)^(a&d)^(c&d),t2=(s0+maj)>>>0;k=j;j=g;g=f;f=(e+t1)>>>0;e=d;d=c;c=a;a=(t1+t2)>>>0;}
   const next=[a,c,d,e,f,g,j,k];for(let i=0;i<8;i++)h[i]=(h[i]+next[i])>>>0;
  }
  const out=new Uint8Array(32),view=new DataView(out.buffer);for(let i=0;i<8;i++)view.setUint32(i*4,h[i]);return out;
 }
 function hmac(key,data){if(key.length>64)key=sha256(key);const a=new Uint8Array(64).fill(0x36),b=new Uint8Array(64).fill(0x5c);for(let i=0;i<key.length;i++){a[i]^=key[i];b[i]^=key[i];}return sha256(concat(b,sha256(concat(a,data))));}
 const hkdf=(key,salt,info)=>hmac(hmac(salt,key),concat(enc.encode(info),Uint8Array.of(1)));
 const le=(value,n=8)=>{let b=new Uint8Array(n);value=BigInt(value);for(let i=0;i<n;i++){b[i]=Number(value&255n);value>>=8n;}return b;};
 const bigint=b=>{let v=0n;for(let i=b.length-1;i>=0;i--)v=(v<<8n)|BigInt(b[i]);return v;};
 function chacha(key,nonce,counter){
  const state=new Uint32Array(16),v=new DataView(key.buffer,key.byteOffset,key.byteLength),n=new DataView(nonce.buffer,nonce.byteOffset,nonce.byteLength);
  state.set([0x61707865,0x3320646e,0x79622d32,0x6b206574]);for(let i=0;i<8;i++)state[i+4]=v.getUint32(i*4,true);state[12]=counter;for(let i=0;i<3;i++)state[13+i]=n.getUint32(i*4,true);
  const x=state.slice(),rot=(a,n)=>(a<<n)|(a>>>(32-n));
  const q=(a,b,c,d)=>{x[a]=(x[a]+x[b])>>>0;x[d]=rot(x[d]^x[a],16);x[c]=(x[c]+x[d])>>>0;x[b]=rot(x[b]^x[c],12);x[a]=(x[a]+x[b])>>>0;x[d]=rot(x[d]^x[a],8);x[c]=(x[c]+x[d])>>>0;x[b]=rot(x[b]^x[c],7);};
  for(let i=0;i<10;i++){q(0,4,8,12);q(1,5,9,13);q(2,6,10,14);q(3,7,11,15);q(0,5,10,15);q(1,6,11,12);q(2,7,8,13);q(3,4,9,14);}
  const out=new Uint8Array(64),view=new DataView(out.buffer);for(let i=0;i<16;i++)view.setUint32(i*4,(x[i]+state[i])>>>0,true);return out;
 }
 function xor(key,nonce,data){const out=new Uint8Array(data.length);for(let at=0;at<data.length;at+=64){const stream=chacha(key,nonce,1+at/64);for(let i=0;i<Math.min(64,data.length-at);i++)out[at+i]=data[at+i]^stream[i];}return out;}
 function poly1305(key,data){const r=bigint(key.slice(0,16))&0x0ffffffc0ffffffc0ffffffc0fffffffn,s=bigint(key.slice(16,32)),p=(1n<<130n)-5n;let a=0n;for(let at=0;at<data.length;at+=16){let part=data.slice(at,at+16);a=((a+bigint(part)+(1n<<BigInt(part.length*8)))*r)%p;}return le((a+s)&((1n<<128n)-1n),16);}
 const pad=b=>new Uint8Array((16-b.length%16)%16);
 const tag=(key,nonce,aad,data)=>poly1305(chacha(key,nonce,0).slice(0,32),concat(aad,pad(aad),data,pad(data),le(aad.length),le(data.length)));
 const hex=b=>Array.from(b,x=>x.toString(16).padStart(2,'0')).join('');
 const unhex=s=>{if(!/^[0-9a-f]{64}$/i.test(s))throw Error('Invalid key');return Uint8Array.from(s.match(/../g),v=>parseInt(v,16));};
 const unbase64=s=>Uint8Array.from(atob(s.replace(/-/g,'+').replace(/_/g,'/')),c=>c.charCodeAt(0));
 class Channel {
  constructor(secret,salt,client=true){this.tx=hkdf(secret,salt,client?'mk-mobile-v2/client-to-daemon':'mk-mobile-v2/daemon-to-client');this.rx=hkdf(secret,salt,client?'mk-mobile-v2/daemon-to-client':'mk-mobile-v2/client-to-daemon');this.txPrefix=enc.encode(client?'MKC2':'MKD2');this.rxPrefix=enc.encode(client?'MKD2':'MKC2');this.sequence=0n;this.received=-1n;}
  seal(plain){if(this.sequence>0xffffffffffffffffn||plain.length>1048580)throw Error('Frame limit');const seq=le(this.sequence++),nonce=concat(this.txPrefix,seq),body=xor(this.tx,nonce,plain);return concat(seq,body,tag(this.tx,nonce,seq,body));}
  open(frame){frame=new Uint8Array(frame);if(frame.length<24||frame.length>1048604)throw Error('Frame size');const seq=frame.slice(0,8),number=bigint(seq);if(number<=this.received)throw Error('Replay rejected');const nonce=concat(this.rxPrefix,seq),body=frame.slice(8,-16),expected=tag(this.rx,nonce,seq,body);let different=0;for(let i=0;i<16;i++)different|=expected[i]^frame[frame.length-16+i];if(different)throw Error('Authentication failed');this.received=number;return xor(this.rx,nonce,body);}
 }
 return {sha256,hmac,hkdf,Channel,concat,hex,unhex,unbase64,enc};
})();
globalThis.MkCrypto=MkCrypto;

// The live stream uses the same postcard DTOs as desktop; no terminal emulation or query replies.
globalThis.MkTerminalCodec=(()=>{
 const C=MkCrypto,decoder=new TextDecoder('utf-8',{fatal:true});
 const v=value=>{let n=BigInt(value),out=[];do{const b=Number(n&127n);n>>=7n;out.push(b|(n?128:0));}while(n);return Uint8Array.from(out);};
 const text=s=>{const b=C.enc.encode(s);return C.concat(v(b.length),b);};
 const frame=(kind,body=new Uint8Array())=>{const h=new Uint8Array(5);new DataView(h.buffer).setUint32(0,body.length+1,true);h[4]=kind;return C.concat(h,body);};
 function reader(bytes){let at=0;return {
  u(){let n=0n,shift=0n;for(let i=0;i<10;i++){if(at>=bytes.length)throw Error('Truncated screen');const b=bytes[at++];n|=BigInt(b&127)<<shift;if(!(b&128))return n;shift+=7n;}throw Error('Invalid screen integer');},
  n(max=Number.MAX_SAFE_INTEGER){const n=Number(this.u());if(!Number.isSafeInteger(n)||n>max)throw Error('Screen limit');return n;},
  byte(){if(at>=bytes.length)throw Error('Truncated screen');return bytes[at++];},
  bool(){const n=this.byte();if(n>1)throw Error('Invalid boolean');return !!n;},
  text(max=4096){const n=this.n(max);if(at+n>bytes.length)throw Error('Truncated screen text');const s=decoder.decode(bytes.slice(at,at+n));at+=n;return s;},
  end(){if(at!==bytes.length)throw Error('Trailing screen data');}
 };}
 function color(r){switch(r.n(2)){case 0:return null;case 1:return {index:r.byte()};case 2:return `rgb(${r.byte()},${r.byte()},${r.byte()})`;}}
 function cells(r){return Array.from({length:r.n(2048)},()=>({ch:r.text(4),zero:Array.from({length:r.n(8)},()=>r.text(4)).join(''),fg:color(r),bg:color(r),underline:color(r),flags:r.n(65535)}));}
 function update(bytes,model){const r=reader(bytes);model.revision=r.u();model.seq=r.u();const done=r.bool(),dirty=new Set();for(let count=r.n(100000);count--;){switch(r.n(7)){
  case 0:{const cols=r.n(2048),lines=r.n(1024);if(!cols||!lines)throw Error('Invalid size');Object.assign(model,{cols,lines,rows:Array.from({length:lines},()=>[]),cursor:{line:0,column:0,visible:true}});for(let i=0;i<lines;i++)dirty.add(i);break;}
  case 1:{const z=r.n(65535),n=(z>>>1)^-(z&1),k=Math.abs(n);if(k>=model.lines)throw Error('Invalid scroll');if(n>0)model.rows=[...model.rows.slice(k),...Array.from({length:k},()=>[])];else model.rows=[...Array.from({length:k},()=>[]),...model.rows.slice(0,model.lines-k)];for(let i=0;i<model.lines;i++)dirty.add(i);break;}
  case 2:{const line=r.n(1023),row=cells(r);if(line>=model.lines||row.length>model.cols)throw Error('Invalid dirty row');model.rows[line]=row;dirty.add(line);break;}
  case 3:{const cursor={line:r.n(1023),column:r.n(2047),visible:r.bool()};if(cursor.line>=model.lines||cursor.column>=model.cols)throw Error('Invalid cursor');model.cursor=cursor;break;}
  case 4:model.title=r.text(1024);break;
  case 5:break;
  case 6:model.cursorStyle={shape:r.n(2),blink:r.bool()};break;
  case 7:r.n(1023);for(let n=r.n(2048);n--;){r.n(2047);r.n(2048);r.text(2048);}break;
 }}r.end();return {done,dirty};}
 function history(bytes){const r=reader(bytes),reply={req:r.n(0xffffffff),total:r.n(0xffffffff),pages:r.n(0xffffffff),skip:r.n(0xffffffff)};reply.rows=Array.from({length:r.n(1000)},()=>cells(r));r.end();return reply;}
 const keys=[['Esc','\x1b'],['Tab','\t'],['⇧Tab','\x1b[Z'],['Ctrl+C','\x03'],['Ctrl+D','\x04'],['↑','\x1b[A'],['↓','\x1b[B'],['←','\x1b[D'],['→','\x1b[C'],['Enter','\r']];
 return {v,text,frame,reader,update,history,keys};
})();
globalThis.MkLiveTerminal=class {
 constructor(credentials,onError){this.credentials=credentials;this.onError=onError;this.generation=0;this.model={cols:80,lines:24,rows:[],cursor:{visible:false}};this.fit=false;this.ready=false;this.inputId=0;this.historyId=0;this.pendingInputs=new Set();this.pendingRoles=[];this.port=document.getElementById('terminal-scroll');this.canvas=document.getElementById('terminal-grid');this.ctx=this.canvas.getContext('2d');this.historyElement=document.getElementById('terminal-history');this.resetHistory();
  document.getElementById('fit-phone').addEventListener('change',e=>{this.setFit(e.target.checked).catch(onError);});
  document.getElementById('load-history').addEventListener('click',()=>this.loadHistory());
  this.port.addEventListener('scroll',()=>{if(this.port.scrollTop<30)this.loadHistory();},{passive:true});
  let start=0;this.port.addEventListener('touchstart',e=>{start=e.touches[0]?.clientY||0;},{passive:true});this.port.addEventListener('touchend',e=>{if(Math.abs((e.changedTouches[0]?.clientY||0)-start)>40&&this.port.scrollTop<30)this.loadHistory();},{passive:true});
  for(const [label,bytes] of MkTerminalCodec.keys){const b=document.createElement('button');b.type='button';b.textContent=label;b.addEventListener('pointerdown',e=>e.preventDefault());b.addEventListener('click',()=>{try{this.input(bytes);}catch(e){onError(e);}});document.getElementById('keybar').append(b);}
  const viewport=()=>{document.documentElement.style.setProperty('--keyboard-inset',`${Math.max(0,innerHeight-(visualViewport?.height||innerHeight)-(visualViewport?.offsetTop||0))}px`);if(this.fit&&this.ready)this.resize().catch(onError);};globalThis.visualViewport?.addEventListener('resize',viewport);globalThis.visualViewport?.addEventListener('scroll',viewport);globalThis.addEventListener('resize',viewport);
  globalThis.addEventListener('pagehide',()=>this.close());
 }
 resetHistory(){this.historySource=null;this.historySkip=0;this.historyPages=null;this.historyPending=null;this.historyRows=[];this.historyEnd=false;if(this.historyElement)this.historyElement.textContent='';const button=document.getElementById('load-history');if(button)button.disabled=false;}
 send(kind,body){if(!this.socket||this.socket.readyState!==WebSocket.OPEN||!this.channel)throw Error('Terminal disconnected');this.socket.send(this.channel.seal(MkTerminalCodec.frame(kind,body)));}
 async open(session){this.close();const generation=this.generation;this.session=session;this.resetHistory();document.getElementById('terminal-panel').hidden=false;document.getElementById('terminal-title').textContent=`Live terminal · #${session}`;
  const T=MkTerminalCodec,C=MkCrypto,socket=new WebSocket(`${location.protocol==='https:'?'wss:':'ws:'}//${location.host}/ws`);this.socket=socket;socket.binaryType='arraybuffer';let phase=0,channel;
  socket.addEventListener('message',e=>{if(generation!==this.generation)return;try{
   if(phase===0){const challenge=JSON.parse(e.data),nonce=crypto.getRandomValues(new Uint8Array(32));if(challenge.version!==2)throw Error('Unsupported terminal protocol');channel=new C.Channel(C.sha256(C.unhex(this.credentials.credential)),C.concat(C.unhex(challenge.challenge),nonce));this.channel=channel;socket.send(JSON.stringify({device_id:this.credentials.device_id,token:null,client_nonce:C.hex(nonce)}));socket.send(channel.seal(C.enc.encode('{"label":"Live terminal"}')));phase=1;return;}
   const bytes=channel.open(e.data);if(phase===1){JSON.parse(new TextDecoder().decode(bytes));phase=2;return;}if(phase===2){const r=T.reader(bytes.slice(5));r.u();r.u();const offered=r.u(),features=offered&((1n<<56n)|1n|4n|4096n);this.send(2,C.concat(T.v(1),T.v(1),T.v(features),T.v(0),T.text('minikrill-mobile-grid')));this.send(0x17,Uint8Array.from([0,2,0]));this.send(0x10,C.concat(T.v(session),Uint8Array.from([0,0])));phase=3;return;}
   if(bytes.length<5||new DataView(bytes.buffer,bytes.byteOffset).getUint32(0,true)!==bytes.length-4)throw Error('Invalid screen frame');const body=bytes.slice(5),r=T.reader(body);
   switch(bytes[4]){
    case 0x20:this.attached={seq:r.u(),revision:r.u()};break;
    case 0x21:{try{const result=T.update(body,this.model);if(result.done){this.ready=true;this.draw();document.getElementById('terminal-status').textContent=`${this.model.cols} × ${this.model.lines}`;}}catch(error){this.send(0x11);throw error;}break;}
    case 0x26:this.receiveHistory(T.history(body));break;
    case 0x27:{const id=r.n(),result=r.n(2);this.pendingInputs.delete(id);if(result)throw Error('Terminal input rejected');break;}
    case 0x2c:{const result=r.n(3),pending=this.pendingRoles.shift();if(pending){clearTimeout(pending.timer);if(result)pending.reject(Error('Terminal resize rejected'));else pending.resolve();}else if(result)throw Error('Terminal role rejected');break;}
    case 0x24:this.ready=false;document.getElementById('terminal-status').textContent='Session ended';break;
    case 0x25:throw Error('Session unavailable');
   }
  }catch(error){this.onError(error);if(phase<3)socket.close();}});
  socket.addEventListener('close',()=>{if(generation!==this.generation)return;const uncertain=this.pendingInputs.size;this.close();document.getElementById('terminal-status').textContent='Disconnected';if(uncertain)this.onError(Error('Input delivery is uncertain; input will not be resent.'));});socket.addEventListener('error',()=>{if(generation===this.generation)this.onError(Error('Live terminal connection failed'));});
 }
 close(){this.generation++;this.ready=false;this.fit=false;document.getElementById('fit-phone').checked=false;this.socket?.close();this.channel=null;this.pendingInputs.clear();for(const p of this.pendingRoles.splice(0)){clearTimeout(p.timer);p.reject(Error('Terminal disconnected'));}}
 input(text){if(!this.ready)throw Error('Wait for the live terminal to connect');const T=MkTerminalCodec,bytes=MkCrypto.enc.encode(text),id=++this.inputId;this.send(0x13,MkCrypto.concat(T.v(id),T.v(bytes.length),bytes));this.pendingInputs.add(id);}
 role(body){return new Promise((resolve,reject)=>{const pending={resolve,reject,timer:setTimeout(()=>{this.socket?.close();reject(Error('Terminal resize timed out'));},5000)};this.pendingRoles.push(pending);try{this.send(0x17,body);}catch(e){this.pendingRoles.pop();clearTimeout(pending.timer);reject(e);}});}
 async setFit(enabled){if(!this.ready){document.getElementById('fit-phone').checked=false;throw Error('Wait for the live terminal to connect');}if(enabled){this.fit=true;try{await this.resize();}catch(e){this.fit=false;document.getElementById('fit-phone').checked=false;throw e;}}else{await this.role(Uint8Array.from([0,2,0]));this.fit=false;}}
 async resize(){const T=MkTerminalCodec,cols=Math.max(10,Math.min(2048,Math.floor((this.port.clientWidth-2)/8))),rows=this.model.lines;await this.role(MkCrypto.concat(T.v(1),T.v(cols),T.v(rows)));}
 loadHistory(){if(!this.ready||this.historyPending||this.historyEnd)return;const T=MkTerminalCodec,req=++this.historyId;this.historyPending={req,source:this.historySource,skip:this.historySkip};const source=this.historySource===null?T.v(0):MkCrypto.concat(T.v(1),T.v(this.historySource));this.send(0x12,MkCrypto.concat(T.v(req),source,T.v(this.historySkip),T.v(100)));document.getElementById('history-status').textContent='Loading earlier rows…';}
 receiveHistory(reply){const pending=this.historyPending;if(!pending||reply.req!==pending.req)return;this.historyPending=null;this.historyPages=reply.pages;this.historySkip=reply.skip+reply.rows.length;this.historyRows=[...reply.rows,...this.historyRows].slice(0,1000);const before=this.port.scrollHeight;this.historyElement.textContent=this.historyRows.map(row=>row.filter(c=>!(c.flags&16)).map(c=>(c.flags&256)?' ':c.ch+c.zero).join('')).join('\n');if(this.historyRows.length)this.historyElement.textContent+='\n';this.port.scrollTop+=this.port.scrollHeight-before;
  const source=pending.source===null?'Hot':`Cold page ${pending.source+1}`;document.getElementById('history-status').textContent=`${source} · ${this.historySkip}/${reply.total} rows`;
  if(this.historySkip>=reply.total||!reply.rows.length){if(pending.source===null)this.historySource=reply.pages-1;else this.historySource=pending.source-1;this.historySkip=0;this.historyEnd=this.historySource<0;}
  document.getElementById('load-history').disabled=this.historyEnd;
 }
 draw(){const m=this.model,ctx=this.ctx,w=8,h=18,scale=devicePixelRatio||1;this.canvas.width=m.cols*w*scale;this.canvas.height=m.lines*h*scale;this.canvas.style.width=`${m.cols*w}px`;this.canvas.style.height=`${m.lines*h}px`;ctx.scale(scale,scale);ctx.fillStyle='#101c1a';ctx.fillRect(0,0,m.cols*w,m.lines*h);ctx.textBaseline='top';const palette=['#172420','#e06c75','#98c379','#e5c07b','#61afef','#c678dd','#56b6c2','#dce9e2','#64736c','#ff7b86','#b6e399','#ffda96','#8dc9ff','#e8a8ff','#8de4ea','#ffffff'];const color=(c,fallback)=>{if(c===null)return fallback;if(typeof c==='string')return c;const n=c.index;if(n<16)return palette[n];if(n>=232){const g=8+(n-232)*10;return `rgb(${g},${g},${g})`;}const k=n-16,levels=[0,95,135,175,215,255];return `rgb(${levels[Math.floor(k/36)]},${levels[Math.floor(k/6)%6]},${levels[k%6]})`;};
  for(let y=0;y<m.lines;y++)for(let x=0;x<(m.rows[y]?.length||0);x++){const c=m.rows[y][x],f=c.flags;if(f&16)continue;const width=f&8?w*2:w;let fg=color(c.fg,'#dce9e2'),bg=color(c.bg,'#101c1a');if(f&4)[fg,bg]=[bg,fg];ctx.fillStyle=bg;ctx.fillRect(x*w,y*h,width,h);if(f&256)continue;ctx.fillStyle=fg;ctx.globalAlpha=f&128?.65:1;ctx.font=`${f&2?'italic ':''}${f&1?'bold ':''}13px monospace`;ctx.fillText(c.ch+c.zero,x*w,y*h+1,width);if(f&32){ctx.fillStyle=color(c.underline,fg);ctx.fillRect(x*w,y*h+15,width,1);}if(f&64)ctx.fillRect(x*w,y*h+8,width,1);ctx.globalAlpha=1;}
  if(m.cursor?.visible){ctx.strokeStyle='#b6e399';ctx.strokeRect(m.cursor.column*w+.5,m.cursor.line*h+.5,w-1,h-1);}this.canvas.setAttribute('aria-label',m.rows.map(row=>row.filter(c=>!(c.flags&16)).map(c=>c.ch+c.zero).join('')).join('\n'));
 }
};

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
 const next=()=>{if(queue.length)return Promise.resolve(queue.shift());if(failed)return Promise.reject(failed);return new Promise((resolve,reject)=>waiters.push({resolve,reject}));};
 const send=bytes=>socket.send(channel.seal(bytes));
 const nextPlain=async()=>channel.open(await next());
 let sequence=Promise.resolve();
 async function rpc(request){const operation=sequence.then(async()=>{send(frame(0x45,C.concat(variable(7),string(JSON.stringify(request)))));const bytes=await nextPlain();if(bytes[4]!==0x65)throw Error('Unexpected daemon response');const r=reader(bytes.slice(5)),kind=Number(r.u());if(kind===4)throw Error(r.text());if(kind!==6)throw Error('Update the desktop for mobile workflows');const reply=JSON.parse(r.text());if(!reply.ok)throw Error(reply.error);return reply.data;});sequence=operation.catch(()=>{});return operation;}
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
 try {
  if(!(ticket&&secretText)&&!credentials)throw Error('Scan a fresh QR code from desktop Settings → Mobile.');
  socket=new WebSocket(`${location.protocol==='https:'?'wss:':'ws:'}//${location.host}/ws`);socket.binaryType='arraybuffer';
  socket.addEventListener('message',e=>{const waiter=waiters.shift();if(waiter)waiter.resolve(e.data);else queue.push(e.data);});
  const terminate=()=>{closed=true;failed=Error('Disconnected. Reopen this page to reconnect.');$('connection').textContent='Disconnected';$('connection').className='connection';for(const w of waiters.splice(0))w.reject(failed);};socket.addEventListener('close',terminate);socket.addEventListener('error',terminate);
  await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});
  const challenge=JSON.parse(await next()),nonce=crypto.getRandomValues(new Uint8Array(32));if(challenge.version!==2)throw Error('Unsupported desktop protocol');
  const pairing=Boolean(ticket&&secretText),secret=pairing?C.unbase64(secretText):C.sha256(C.unhex(credentials.credential));
  channel=new C.Channel(secret,C.concat(C.unhex(challenge.challenge),nonce));secret.fill(0);
  socket.send(JSON.stringify({token:pairing?ticket:null,device_id:pairing?null:credentials.device_id,client_nonce:C.hex(nonce)}));send(C.enc.encode(JSON.stringify({label:/iPhone|iPad/.test(navigator.userAgent)?'iPhone / iPad (Safari)':'Mobile browser'})));
  const authenticated=JSON.parse(decode.decode(await nextPlain()));if(authenticated.credential){credentials=authenticated;localStorage.setItem('minikrill-mobile-v2',JSON.stringify(credentials));}
  parameters.delete('pair');history.replaceState({},'',location.pathname+(parameters.size?'?'+parameters.toString():''));
  const hello=reader((await nextPlain()).slice(5));hello.u();hello.u();const features=hello.u();send(frame(2,C.concat(variable(1),variable(1),variable(features),variable(0),string('minikrill-mobile-pwa'))));
  terminal=new MkLiveTerminal(credentials,error);globalThis.liveTerminal=terminal;await refresh();if(parameters.has('session')&&selected)terminal.open(selected);scheduleRefresh();
 }catch(e){error(e);if(socket?.readyState===WebSocket.OPEN)socket.close();}
})();
