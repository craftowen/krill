// Orca mobile interaction/theme reference: MIT, Copyright (c) 2026 Lovecast Inc.
// See docs/research/orca-mobile-ui-spec.md §5.1 for the MIT notice.
// Icons: Lucide (ISC), Copyright (c) Lucide Contributors, from gpui-kit-assets.

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
 return {v,text,frame,reader,update,history};
})();

(()=>{
 'use strict';
 const T=MkTerminalCodec,C=MkCrypto,UI_KEY='krill-mobile-ui-v1';
 const SCALES=Object.freeze([.5,.75,1,1.25,1.5,2]),drafts=new Map(),modes=new Map();
 const KEYS=Object.freeze([
  ['escape','Esc','\x1b'],['tab','Tab','\t'],['enter','Enter','\r'],['shiftTab','Shift+Tab','\x1b[Z'],['space','Space',' '],
  ['backspace','⌫','\x7f',true],['delete','Del','\x1b[3~',true],['arrowUp','↑','\x1b[A',true],['arrowDown','↓','\x1b[B',true],
  ['arrowLeft','←','\x1b[D',true],['arrowRight','→','\x1b[C',true],['ctrlC','Ctrl+C','\x03'],['ctrlD','Ctrl+D','\x04'],
  ['ctrlL','Ctrl+L','\x0c'],['ctrlZ','Ctrl+Z','\x1a'],['ctrlR','Ctrl+R','\x12'],['ctrlA','Ctrl+A','\x01'],
  ['ctrlE','Ctrl+E','\x05'],['ctrlW','Ctrl+W','\x17'],['ctrlU','Ctrl+U','\x15']
 ].map(([id,label,bytes,repeatable=false])=>Object.freeze({id,label,bytes,repeatable})));
 const icons={"monitor":[["rect",{"width":"20","height":"14","x":"2","y":"3","rx":"2"}],["line",{"x1":"8","x2":"16","y1":"21","y2":"21"}],["line",{"x1":"12","x2":"12","y1":"17","y2":"21"}]],"smartphone":[["rect",{"width":"14","height":"20","x":"5","y":"2","rx":"2","ry":"2"}],["path",{"d":"M12 18h.01"}]],"direct":[["path",{"d":"m6 17 5-5-5-5"}],["path",{"d":"m13 17 5-5-5-5"}]],"keyboard":[["path",{"d":"M10 8h.01"}],["path",{"d":"M12 12h.01"}],["path",{"d":"M14 8h.01"}],["path",{"d":"M16 12h.01"}],["path",{"d":"M18 8h.01"}],["path",{"d":"M6 8h.01"}],["path",{"d":"M7 16h10"}],["path",{"d":"M8 12h.01"}],["rect",{"width":"20","height":"16","x":"2","y":"4","rx":"2"}]],"dismiss":[["path",{"d":"M 20 4 A2 2 0 0 1 22 6"}],["path",{"d":"M 22 6 L 22 16.41"}],["path",{"d":"M 7 16 L 16 16"}],["path",{"d":"M 9.69 4 L 20 4"}],["path",{"d":"M14 8h.01"}],["path",{"d":"M18 8h.01"}],["path",{"d":"m2 2 20 20"}],["path",{"d":"M20 20H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2"}],["path",{"d":"M6 8h.01"}],["path",{"d":"M8 12h.01"}]],"send":[["path",{"d":"m5 12 7-7 7 7"}],["path",{"d":"M12 19V5"}]],"photo":[["rect",{"width":"18","height":"18","x":"3","y":"3","rx":"2","ry":"2"}],["circle",{"cx":"9","cy":"9","r":"2"}],["path",{"d":"m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"}]]};
 const make=(tag,cls,text)=>{const el=document.createElement(tag);if(cls)el.className=cls;if(text!==undefined)el.textContent=text;return el;};
 function icon(name){const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');for(const [k,v] of Object.entries({viewBox:'0 0 24 24',width:'16',height:'16',fill:'none',stroke:'currentColor','stroke-width':'2','stroke-linecap':'round','stroke-linejoin':'round','aria-hidden':'true'}))svg.setAttribute(k,v);for(const [tag,attrs] of icons[name]){const shape=document.createElementNS(svg.namespaceURI,tag);for(const [key,value] of Object.entries(attrs))shape.setAttribute(key,value);svg.append(shape);}return svg;}
 function button(cls,label,svg){const el=make('button',cls,svg?undefined:label);el.type='button';el.setAttribute('aria-label',label);el.title=label;if(svg)el.append(icon(svg));return el;}
 function settings(){try{const value=JSON.parse(localStorage.getItem(UI_KEY)||'{}');return value&&typeof value==='object'&&!Array.isArray(value)?value:{};}catch{return {};}}
 const snap=value=>SCALES.reduce((best,n)=>Math.abs(n-value)<Math.abs(best-value)?n:best,1);
 class Terminal {
  constructor(root,options){
   if(!root||!Number.isSafeInteger(options.sessionId)||options.sessionId<=0||typeof options.connect!=='function')throw Error('Invalid terminal mount');
   this.root=root;this.options=options;this.session=options.sessionId;this.generation=1;this.inputGeneration=1;this.disposed=false;
   this.abort=new AbortController();this.model={cols:80,lines:24,rows:[],cursor:{visible:false}};this.fit=false;this.ready=false;this.fontScale=snap(Number(options.preferences?.textScale)||1);
   this.dirty=new Set();this.full=true;this.inputId=0;this.historyId=0;this.pendingInputs=new Map();this.pendingRoles=[];this.deferredKeys=[];this.phase='connecting';
   const stored=settings().terminalInputModes?.[this.session],initial=options.preferences?.inputMode||modes.get(this.session)||stored;
   this.inputMode=initial==='buffered'?'buffered':'direct';modes.set(this.session,this.inputMode);
   this.build();this.resetHistory();this.bindInput();this.bindViewport();this.setFontScale(this.fontScale,false);this.updateMode();this.status('connecting','Connecting…');
   this.open();
  }
  listen(el,event,fn,extra={}){el.addEventListener(event,fn,{...extra,signal:this.abort.signal});}
  guard(action){try{Promise.resolve(action()).catch(e=>{if(!this.disposed)this.error(e);});}catch(e){this.error(e);}}
  status(phase,message,error){if(this.disposed)return;this.phase=phase;this.statusEl.textContent=message;this.statusEl.setAttribute('role',error?'alert':'status');this.options.onStatus?.({phase,message,error,fit:this.fit,inputMode:this.inputMode,fontScale:this.fontScale});}
  error(e){this.status('error',e.message||String(e),e);}
  build(){
   this.root.classList.add('terminal-mount');this.frame=make('div','terminal-frame');this.port=make('div','terminal-scroll');this.port.tabIndex=0;
   this.historyElement=make('pre','terminal-history');this.canvas=make('canvas','terminal-grid');this.canvas.setAttribute('role','img');this.ctx=this.canvas.getContext('2d');
   this.port.append(this.historyElement,this.canvas);this.statusEl=make('div','terminal-status');this.statusEl.dataset.testid='terminal-status';
   const controls=make('div','terminal-history-controls');this.historyButton=button('load-history','Load earlier output');this.historyStatus=make('span','history-status');this.historyStatus.setAttribute('role','status');controls.append(this.historyButton,this.historyStatus);this.frame.append(this.port,this.statusEl,controls);
   this.dock=make('footer','command-dock');const accessory=make('div','accessory-row');this.dismiss=button('dismiss-keyboard','Dismiss keyboard','dismiss');this.dismiss.hidden=true;
   const keybar=make('div','keybar');keybar.setAttribute('role','toolbar');keybar.setAttribute('aria-label','Terminal keys');this.fitButton=button('accessory-key display-mode','Fit to Phone','monitor');this.fitButton.setAttribute('aria-pressed','false');this.directButton=button('accessory-key direct-toggle','Direct input','direct');keybar.append(this.fitButton,this.directButton);
   const ordered=this.options.preferences?.orderedKeyIds||[],visible=this.options.preferences?.visibleKeyIds;
   const ids=[...new Set([...ordered,...KEYS.map(k=>k.id)])];for(const id of ids){const key=KEYS.find(k=>k.id===id);if(!key||visible&&!visible.includes(id))continue;const b=button('accessory-key',key.label);b.dataset.keyId=id;b.dataset.testid=`terminal-key-${id}`;keybar.append(b);this.bindKey(b,key);}
   accessory.append(this.dismiss,keybar);const inputRow=make('div','input-row');this.live=button('live-focus-target','Show keyboard for live terminal input','keyboard');const copy=make('span','live-input-copy');copy.append(make('strong',null,'Live input'),make('small',null,'Tap to show keyboard'));this.live.append(copy);
   this.capture=make('textarea','live-capture');this.capture.setAttribute('aria-label','Terminal direct input');this.capture.setAttribute('autocapitalize','none');this.capture.setAttribute('autocorrect','off');this.capture.setAttribute('autocomplete','off');this.capture.spellcheck=false;
   this.form=make('form','buffered-command');this.command=make('input','command-input');this.command.placeholder='Type a command…';this.command.setAttribute('aria-label','Command');this.command.autocomplete='off';this.command.setAttribute('autocapitalize','none');this.command.spellcheck=false;this.command.value=drafts.get(this.session)||'';
   this.sendButton=button('command-send','Send command','send');this.sendButton.type='submit';this.form.append(this.command,this.sendButton);inputRow.append(this.live,this.capture,this.form);
   if(this.options.onPhoto){const photo=button('photo-attach','Attach photo','photo'),images=make('input','photo-picker'),files=make('input','file-picker');images.type=files.type='file';images.accept='image/*';images.hidden=files.hidden=true;inputRow.append(photo,images,files);let held=false,timer;
    this.listen(photo,'pointerdown',()=>{held=false;timer=setTimeout(()=>{held=true;files.click();},350);});for(const event of ['pointerup','pointercancel','pointerleave'])this.listen(photo,event,()=>clearTimeout(timer));this.listen(photo,'click',()=>{if(!held)images.click();});this.listen(this.abort.signal,'abort',()=>clearTimeout(timer));
    for(const picker of [images,files])this.listen(picker,'change',()=>{const file=picker.files?.[0];if(file)this.guard(()=>this.options.onPhoto(file));picker.value='';});
   }
   this.dock.append(accessory,inputRow);this.root.append(this.frame,this.dock);
   this.listen(this.historyButton,'click',()=>this.guard(()=>this.loadHistory()));this.listen(this.port,'scroll',()=>{if(this.port.scrollTop<30)this.guard(()=>this.loadHistory());},{passive:true});
   this.listen(this.fitButton,'pointerdown',e=>e.preventDefault());this.listen(this.directButton,'pointerdown',e=>e.preventDefault());
   this.listen(this.fitButton,'click',()=>this.guard(()=>this.fitToPhone()));this.listen(this.directButton,'click',()=>this.setInputMode(this.inputMode==='direct'?'buffered':'direct'));
   this.listen(this.dismiss,'click',()=>{this.capture.blur();this.command.blur();});this.listen(this.live,'click',()=>this.focus());
   let moved=false;this.listen(this.port,'pointerdown',()=>{moved=false;});this.listen(this.port,'pointermove',()=>{moved=true;});this.listen(this.port,'click',()=>{if(!moved&&!this.pinching)this.focus();});
   let pinch;const distance=touches=>Math.hypot(touches[0].clientX-touches[1].clientX,touches[0].clientY-touches[1].clientY);
   this.listen(this.port,'touchstart',e=>{if(e.touches.length===2){pinch={distance:distance(e.touches),scale:this.fontScale};this.pinching=true;}},{passive:true});
   this.listen(this.port,'touchmove',e=>{if(pinch&&e.touches.length===2){e.preventDefault();this.previewScale=Math.max(.5,Math.min(2,pinch.scale*distance(e.touches)/pinch.distance));this.canvas.style.transformOrigin='top left';this.canvas.style.transform=`scale(${this.previewScale/this.fontScale})`;}},{passive:false});
   const finish=()=>{if(!pinch)return;pinch=null;this.canvas.style.transform='';this.setFontScale(this.previewScale||this.fontScale);setTimeout(()=>{this.pinching=false;},0);};this.listen(this.port,'touchend',finish);this.listen(this.port,'touchcancel',finish);
  }
  stopRepeat(){clearTimeout(this.repeatTimer);this.repeatToken=(this.repeatToken||0)+1;this.heldPointer=null;}
  bindKey(el,key){
   this.listen(el,'pointerdown',e=>{e.preventDefault();if(e.button!==0)return;this.stopRepeat();if(!key.repeatable)return;this.heldPointer=e.pointerId;el.setPointerCapture?.(e.pointerId);const token=this.repeatToken;
    const tick=async()=>{try{await this.sendKey(key.id);if(token===this.repeatToken&&!this.disposed)this.repeatTimer=setTimeout(tick,45);}catch(e){this.stopRepeat();this.error(e);}};
    const initial=this.sendKey(key.id);initial.catch(e=>{this.stopRepeat();this.error(e);});this.repeatTimer=setTimeout(()=>{initial.then(()=>{if(token===this.repeatToken&&!this.disposed)tick();},()=>{});},400);
   });
   this.listen(el,'click',e=>{if(!key.repeatable||e.detail===0)this.guard(()=>this.sendKey(key.id));});
   for(const event of ['pointerup','pointercancel','lostpointercapture','pointerleave'])this.listen(el,event,()=>this.stopRepeat());
  }
  bindInput(){
   this.listen(this.capture,'focus',()=>{this.capture.value='';this.inputArmed=true;});
   this.listen(this.capture,'compositionstart',()=>{if(this.inputMode!=='direct'||this.disposed)return;this.composing=true;this.compositionGeneration=this.inputGeneration;this.compositionConnection=this.generation;});
   this.listen(this.capture,'compositionend',e=>{
    const valid=this.inputArmed&&this.composing&&this.compositionGeneration===this.inputGeneration&&this.compositionConnection===this.generation&&this.inputMode==='direct';this.composing=false;
    const text=e.data||this.capture.value;this.capture.value='';if(!valid){this.ignoreCommit=text;clearTimeout(this.commitTimer);this.commitTimer=setTimeout(()=>{this.ignoreCommit=null;},0);this.deferredKeys.splice(0).forEach(p=>p.reject(Error('Input target changed')));return;}
    this.ignoreCommit=text;clearTimeout(this.commitTimer);this.commitTimer=setTimeout(()=>{this.ignoreCommit=null;},0);
    if(text)this.guard(()=>this.input(text));for(const pending of this.deferredKeys.splice(0))this.input(pending.bytes).then(pending.resolve,pending.reject);
   });
   this.listen(this.capture,'input',e=>{if(this.disposed||!this.inputArmed||this.inputMode!=='direct'){this.capture.value='';return;}if(this.composing||e.isComposing)return;const text=this.capture.value;this.capture.value='';if(this.ignoreCommit&&(!text||text===this.ignoreCommit)){this.ignoreCommit=null;return;}if(text)this.guard(()=>this.input(text));});
   this.listen(this.capture,'beforeinput',e=>{if(!this.inputArmed||this.composing||e.isComposing||this.inputMode!=='direct')return;const bytes={deleteContentBackward:'\x7f',deleteContentForward:'\x1b[3~',insertLineBreak:'\r',insertParagraph:'\r'}[e.inputType];if(bytes){e.preventDefault();this.guard(()=>this.input(bytes));}});
   this.listen(this.capture,'keydown',e=>{if(!this.inputArmed||this.inputMode!=='direct'||this.composing||e.isComposing||e.keyCode===229)return;let bytes;const special={Escape:'escape',Tab:e.shiftKey?'shiftTab':'tab',Enter:'enter',Backspace:'backspace',Delete:'delete',ArrowUp:'arrowUp',ArrowDown:'arrowDown',ArrowLeft:'arrowLeft',ArrowRight:'arrowRight'}[e.key];if(special)bytes=KEYS.find(k=>k.id===special).bytes;else if(e.ctrlKey&&!e.altKey&&!e.metaKey&&/^[a-z]$/i.test(e.key))bytes=String.fromCharCode(e.key.toUpperCase().charCodeAt(0)-64);if(bytes){e.preventDefault();this.guard(()=>this.input(bytes));}});
   this.listen(this.command,'compositionstart',()=>{this.bufferedComposing=true;});this.listen(this.command,'compositionend',()=>{this.bufferedComposing=false;});
   this.listen(this.command,'keydown',e=>{if(e.key==='Enter'&&(e.isComposing||this.bufferedComposing||e.keyCode===229))e.preventDefault();});
   this.listen(this.command,'input',()=>drafts.set(this.session,this.command.value));
   this.listen(this.form,'submit',e=>{e.preventDefault();if(e.isComposing||this.bufferedComposing||this.bufferedPending)return;const value=this.command.value;if(!value)return;const generation=this.generation;this.bufferedPending=true;this.sendButton.disabled=true;
    this.guard(async()=>{try{await this.input(value+'\r');if(!this.disposed&&generation===this.generation&&this.command.value===value){this.command.value='';drafts.delete(this.session);}}finally{this.bufferedPending=false;this.sendButton.disabled=false;}});
   });
  }
  updateMode(){this.root.dataset.inputMode=this.inputMode;this.live.hidden=this.capture.hidden=this.inputMode!=='direct';this.form.hidden=this.inputMode==='direct';this.directButton.setAttribute('aria-pressed',String(this.inputMode==='direct'));}
  setInputMode(mode){if(this.disposed||!['direct','buffered'].includes(mode)||mode===this.inputMode)return;this.invalidateInput();this.inputMode=mode;modes.set(this.session,mode);const value=settings();value.terminalInputModes={...value.terminalInputModes,[this.session]:mode};try{localStorage.setItem(UI_KEY,JSON.stringify(value));}catch{}this.options.onPreferenceChange?.({inputMode:mode,sessionId:this.session});this.updateMode();}
  getInputMode(){return this.inputMode;}
  focus(){if(!this.disposed)(this.inputMode==='direct'?this.capture:this.command).focus({preventScroll:true});}
  invalidateInput(){this.inputGeneration++;this.inputArmed=false;this.composing=false;this.capture.value='';this.capture.blur();this.command.blur();this.stopRepeat();this.deferredKeys.splice(0).forEach(p=>p.reject(Error('Input target changed')));}
  sendKey(id){const key=KEYS.find(k=>k.id===id);if(!key)return Promise.reject(Error('Unknown terminal key'));if(this.disposed)return Promise.reject(Error('Input target changed'));if(this.composing)return new Promise((resolve,reject)=>this.deferredKeys.push({bytes:key.bytes,resolve,reject}));return this.input(key.bytes);}
  bindViewport(){
   const viewport=()=>{const vv=globalThis.visualViewport,inset=vv&&Math.abs(vv.scale-1)<.05?Math.max(0,innerHeight-vv.height-vv.offsetTop):0;this.root.style.setProperty('--keyboard-inset',`${inset}px`);this.root.dataset.keyboardOpen=String(inset>80);this.dismiss.hidden=inset<=80;if(this.fit&&this.ready&&!this.fitPending)this.guard(()=>this.resize());};
   if(globalThis.visualViewport){this.listen(visualViewport,'resize',viewport);this.listen(visualViewport,'scroll',viewport);}this.listen(globalThis,'resize',viewport);this.listen(globalThis,'pagehide',()=>this.disconnect());this.listen(globalThis,'blur',()=>this.stopRepeat());viewport();
   if(globalThis.ResizeObserver){this.observer=new ResizeObserver(()=>{if(this.fit&&this.ready&&!this.fitPending)this.guard(()=>this.resize());});this.observer.observe(this.port);}
  }
  setFontScale(scale,notify=true){if(this.disposed)return;this.fontScale=snap(Number(scale)||1);this.fontSize=Math.max(6,Math.round(13*this.fontScale));this.cellWidth=8*this.fontSize/13;this.cellHeight=18*this.fontSize/13;this.historyElement.style.fontSize=`${this.fontSize}px`;this.historyElement.style.lineHeight=`${this.cellHeight}px`;this.schedule(true);if(notify)this.options.onPreferenceChange?.({textScale:this.fontScale});if(this.fit&&this.ready&&!this.fitPending)this.guard(()=>this.resize());}
  async open(){
   const generation=this.generation;let result;try{result=await this.options.connect({sessionId:this.session});}catch(e){if(generation===this.generation)this.disconnect('disconnected',e.message);return;}
   const {socket,credentials}=result||{};if(this.disposed||generation!==this.generation){socket?.close();return;}if(!socket||!credentials){this.error(Error('Invalid terminal connection factory'));return;}
   this.socket=socket;this.connectionTimer=setTimeout(()=>{if(generation===this.generation)this.disconnect('disconnected','Terminal connection timed out');},15000);socket.binaryType='arraybuffer';let phase=0,channel;
   this.listen(socket,'message',e=>{if(generation!==this.generation||this.disposed)return;try{
    if(phase===0){const challenge=JSON.parse(e.data),nonce=crypto.getRandomValues(new Uint8Array(32));if(challenge.version!==2)throw Error('Unsupported terminal protocol');channel=new C.Channel(C.sha256(C.unhex(credentials.credential)),C.concat(C.unhex(challenge.challenge),nonce));this.channel=channel;socket.send(JSON.stringify({device_id:credentials.device_id,token:null,client_nonce:C.hex(nonce)}));socket.send(channel.seal(C.enc.encode('{"label":"Live terminal"}')));
     // 게이트웨이는 proof만 읽고 나머지를 IPC로 넘긴다(협상은 교집합). ServerHello를 기다리지 않고 Hello·role·attach를 함께 보낸다.
     this.send(2,C.concat(T.v(1),T.v(1),T.v((1n<<56n)|1n|4n|4096n),T.v(0),T.text('minikrill-mobile-grid')));this.guard(()=>this.role(Uint8Array.from([0,2,0])));this.send(0x10,C.concat(T.v(this.session),Uint8Array.from([0,0])));phase=1;return;}
    const bytes=channel.open(e.data);if(phase===1){JSON.parse(new TextDecoder().decode(bytes));phase=2;return;}
    if(phase===2){if(bytes[4]!==1)throw Error('Unexpected terminal handshake');phase=3;return;}
    if(bytes.length<5||new DataView(bytes.buffer,bytes.byteOffset).getUint32(0,true)!==bytes.length-4)throw Error('Invalid screen frame');const body=bytes.slice(5),r=T.reader(body);
    switch(bytes[4]){
     case 0x20:this.attached={seq:r.u(),revision:r.u()};break;
     case 0x21:{try{const result=T.update(body,this.model);for(const y of result.dirty)this.dirty.add(y);if(result.done){clearTimeout(this.connectionTimer);this.ready=true;this.schedule();this.status('ready',`${this.model.cols} × ${this.model.lines}`);if(this.model.title!==this.title){this.title=this.model.title;this.options.onTitle?.(this.title||'');}}}catch(error){this.send(0x11);throw error;}break;}
     case 0x26:this.receiveHistory(T.history(body));break;
     case 0x27:{const id=r.n(),result=r.n(2),pending=this.pendingInputs.get(id);this.pendingInputs.delete(id);if(result){const error=Error('Terminal input rejected');pending?.reject(error);this.error(error);}else pending?.resolve();break;}
     case 0x2c:{const result=r.n(3),pending=this.pendingRoles.shift();if(pending){clearTimeout(pending.timer);if(result)pending.reject(Error('Terminal resize rejected'));else pending.resolve();}else if(result)throw Error('Terminal role rejected');break;}
     case 0x24:this.disconnect('ended','Session ended');break;
     case 0x25:this.disconnect('error','Session unavailable');break;
    }
   }catch(error){this.error(error);if(phase<3)socket.close();}});
   this.listen(socket,'close',e=>{if(generation===this.generation)this.disconnect(e.code===1008?'denied':'disconnected',e.code===1008?'Terminal authentication rejected':'Disconnected');});this.listen(socket,'error',()=>{if(generation===this.generation)this.disconnect('disconnected','Live terminal connection failed');});
  }
  send(kind,body){if(!this.socket||this.socket.readyState!==1||!this.channel)throw Error('Terminal disconnected');this.socket.send(this.channel.seal(T.frame(kind,body)));}
  input(text){if(this.disposed||!this.ready)return Promise.reject(Error('Wait for the live terminal to connect'));const bytes=C.enc.encode(text),id=++this.inputId;return new Promise((resolve,reject)=>{this.pendingInputs.set(id,{resolve,reject});try{this.send(0x13,C.concat(T.v(id),T.v(bytes.length),bytes));}catch(e){this.pendingInputs.delete(id);reject(e);}});}
  role(body){return new Promise((resolve,reject)=>{const pending={resolve,reject,timer:setTimeout(()=>{this.socket?.close();this.disconnect('disconnected','Terminal resize timed out');},5000)};this.pendingRoles.push(pending);try{this.send(0x17,body);}catch(e){this.pendingRoles.pop();clearTimeout(pending.timer);reject(e);}});}
  async fitToPhone(){if(!this.ready)throw Error('Wait for the live terminal to connect');if(this.fitPending)return;const enabled=!this.fit,generation=this.generation;this.fitPending=true;this.fitButton.disabled=true;try{if(enabled)await this.resize();else await this.role(Uint8Array.from([0,2,0]));if(this.disposed||generation!==this.generation)return;this.fit=enabled;this.fitButton.setAttribute('aria-pressed',String(enabled));this.fitButton.replaceChildren(icon(enabled?'smartphone':'monitor'));this.status('ready',`${this.model.cols} × ${this.model.lines}`);}finally{this.fitPending=false;this.fitButton.disabled=false;}}
  async resize(){const cols=Math.max(10,Math.min(2048,Math.floor((this.port.clientWidth-2)/this.cellWidth))),rows=this.model.lines;if(this.resizePending)return this.resizePending;this.resizePending=this.role(C.concat(T.v(1),T.v(cols),T.v(rows)));try{await this.resizePending;}finally{this.resizePending=null;}}
  resetHistory(){this.historySource=null;this.historySkip=0;this.historyPending=null;this.historyRows=[];this.historyEnd=false;this.historyElement.textContent='';}
  loadHistory(){if(!this.ready||this.historyPending||this.historyEnd||this.disposed)return;const req=++this.historyId,source=this.historySource===null?T.v(0):C.concat(T.v(1),T.v(this.historySource));this.send(0x12,C.concat(T.v(req),source,T.v(this.historySkip),T.v(Math.min(100,1000-this.historyRows.length))));this.historyPending={req,source:this.historySource,skip:this.historySkip};this.historyStatus.textContent='Loading earlier rows…';}
  receiveHistory(reply){const pending=this.historyPending;if(!pending||reply.req!==pending.req)return;this.historyPending=null;this.historySkip=reply.skip+reply.rows.length;const before=this.port.scrollHeight;this.historyRows=[...reply.rows,...this.historyRows].slice(-1000);this.historyElement.textContent=this.historyRows.map(row=>row.filter(c=>!(c.flags&16)).map(c=>(c.flags&256)?' ':c.ch+c.zero).join('')).join('\n');if(this.historyRows.length)this.historyElement.textContent+='\n';this.port.scrollTop+=this.port.scrollHeight-before;this.historyStatus.textContent=`${pending.source===null?'Hot':`Cold page ${pending.source+1}`} · ${this.historySkip}/${reply.total} rows`;if(this.historySkip>=reply.total||!reply.rows.length){this.historySource=pending.source===null?reply.pages-1:pending.source-1;this.historySkip=0;this.historyEnd=this.historySource<0;}if(this.historyRows.length>=1000)this.historyEnd=true;this.historyButton.disabled=this.historyEnd;}
  // 갱신은 dirty 행으로 모아 프레임당 한 번 그린다. 크기·배율이 바뀌거나 full이면 전체, 아니면 dirty 행(+커서 이동 전후 행)만.
  schedule(full=false){if(full)this.full=true;if(this.rafId||this.disposed)return;const raf=globalThis.requestAnimationFrame||(fn=>setTimeout(fn,16));this.rafId=raf(()=>{this.rafId=0;if(!this.disposed)this.draw();});}
  draw(){
   const m=this.model,ctx=this.ctx,w=this.cellWidth||8,h=this.cellHeight||18,font=this.fontSize||13,cssW=m.cols*w,cssH=m.lines*h;
   // iOS 캔버스 면적 한도(약 16.7M px, 추정) 아래로 배율을 묶는다.
   const scale=Math.min(globalThis.devicePixelRatio||1,Math.sqrt(16e6/(cssW*cssH))),width=Math.round(cssW*scale),height=Math.round(cssH*scale);
   let full=this.full||this.canvas.width!==width||this.canvas.height!==height;this.full=false;
   if(this.canvas.width!==width||this.canvas.height!==height){this.canvas.width=width;this.canvas.height=height;this.font=null;}
   if(this.cssSize!==`${cssW}x${cssH}`){this.cssSize=`${cssW}x${cssH}`;this.canvas.style.width=`${cssW}px`;this.canvas.style.height=`${cssH}px`;}
   ctx.setTransform(scale,0,0,scale,0,0);ctx.textBaseline='top';
   const cursor=m.cursor?.visible?{line:m.cursor.line,column:m.cursor.column,shape:m.cursorStyle?.shape??2}:null,cursorKey=JSON.stringify(cursor),dirty=this.dirty;this.dirty=new Set();
   if(cursorKey!==this.cursorKey){if(this.cursorLine!=null)dirty.add(this.cursorLine);if(cursor)dirty.add(cursor.line);}this.cursorKey=cursorKey;this.cursorLine=cursor?.line;
   const palette=['#15161e','#f7768e','#9ece6a','#e0af68','#7aa2f7','#bb9af7','#7dcfff','#a9b1d6','#414868','#f7768e','#9ece6a','#e0af68','#7aa2f7','#bb9af7','#7dcfff','#c0caf5'];
   const color=(c,fallback)=>{if(c==null)return fallback;if(typeof c==='string')return c;const n=c.index;if(n<16)return palette[n];if(n>=232){const g=8+(n-232)*10;return `rgb(${g},${g},${g})`;}const k=n-16,levels=[0,95,135,175,215,255];return `rgb(${levels[Math.floor(k/36)]},${levels[Math.floor(k/6)%6]},${levels[k%6]})`;};
   ctx.fillStyle='#1a1b26';if(full)ctx.fillRect(0,0,cssW,cssH);else for(const y of dirty)if(y<m.lines)ctx.fillRect(0,y*h,cssW,h);
   for(let y=0;y<m.lines;y++){if(!full&&!dirty.has(y))continue;for(let x=0;x<(m.rows[y]?.length||0);x++){const c=m.rows[y][x],f=c.flags;if(f&16)continue;const width=f&8?w*2:w;let fg=color(c.fg,'#c0caf5'),bg=color(c.bg,'#1a1b26');if(f&4)[fg,bg]=[bg,fg];ctx.fillStyle=bg;ctx.fillRect(x*w,y*h,width,h);if(f&256)continue;ctx.fillStyle=fg;ctx.globalAlpha=f&128?.65:1;const face=`${f&2?'italic ':''}${f&1?'500':'300'} ${font}px ui-monospace, "SF Mono", Menlo, Monaco, "Cascadia Mono", Consolas, "DejaVu Sans Mono", "Liberation Mono", "Symbols Nerd Font Mono", monospace`;if(face!==this.font){ctx.font=face;this.font=face;}ctx.fillText(c.ch+c.zero,x*w,y*h+font/13,width);if(f&32){ctx.fillStyle=color(c.underline,fg);ctx.fillRect(x*w,y*h+h-3*font/13,width,font/13);}if(f&64)ctx.fillRect(x*w,y*h+h/2,width,font/13);ctx.globalAlpha=1;}}
   if(cursor&&(full||dirty.has(cursor.line))){ctx.fillStyle='#c0caf5';const x=cursor.column*w,y=cursor.line*h;if(cursor.shape===2)ctx.fillRect(x,y,Math.max(1,font/13),h);else if(cursor.shape===1)ctx.fillRect(x,y+h-2,w,2);else{ctx.strokeStyle='#c0caf5';ctx.strokeRect(x+.5,y+.5,w-1,h-1);}}
   // 화면 전체 텍스트를 매 프레임 만들지 않도록 접근성 라벨은 최대 500ms마다 갱신한다.
   this.labelTimer??=setTimeout(()=>{this.labelTimer=null;if(!this.disposed)this.canvas.setAttribute('aria-label',this.model.rows.map(row=>row.filter(c=>!(c.flags&16)).map(c=>(c.flags&256)?' ':c.ch+c.zero).join('')).join('\n'));},500);
  }
  disconnect(phase='disconnected',message='Disconnected'){
   clearTimeout(this.connectionTimer);this.generation++;this.ready=false;this.fit=false;this.invalidateInput();const uncertain=this.pendingInputs.size,error=Error('Input delivery is uncertain; input will not be resent.');for(const pending of this.pendingInputs.values())pending.reject(error);this.pendingInputs.clear();for(const pending of this.pendingRoles.splice(0)){clearTimeout(pending.timer);pending.reject(Error('Terminal disconnected'));}this.channel=null;this.socket?.close();this.fitButton.setAttribute('aria-pressed','false');this.fitButton.replaceChildren(icon('monitor'));this.status(phase,uncertain?error.message:message,uncertain?error:undefined);
  }
  unmount(){if(this.disposed)return;drafts.set(this.session,this.command.value);this.disconnect();this.disposed=true;this.abort.abort();this.observer?.disconnect();clearTimeout(this.commitTimer);clearTimeout(this.labelTimer);if(this.rafId)(globalThis.cancelAnimationFrame||clearTimeout)(this.rafId);this.frame.remove();this.dock.remove();this.root.classList.remove('terminal-mount');this.root.style.removeProperty('--keyboard-inset');delete this.root.dataset.keyboardOpen;delete this.root.dataset.inputMode;}
 }
 const mobile=globalThis.KrillMobile||(globalThis.KrillMobile={});
 mobile.terminal=Object.freeze({keys:KEYS,fontScales:SCALES,mount(root,options){const instance=new Terminal(root,options);return Object.freeze(Object.fromEntries(['unmount','focus','setFontScale','setInputMode','getInputMode','sendKey','fitToPhone','loadHistory'].map(name=>[name,instance[name].bind(instance)])));}});
})();
