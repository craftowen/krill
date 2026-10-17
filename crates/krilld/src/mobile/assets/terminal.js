
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
