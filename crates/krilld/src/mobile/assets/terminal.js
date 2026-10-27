// Orca mobile interaction/theme reference: MIT, Copyright (c) 2026 Lovecast Inc.
// See docs/research/orca-mobile-ui-spec.md §5.1 for the MIT notice.
// Icons: Lucide (ISC), Copyright (c) Lucide Contributors, from gpui-kit-assets.


(()=>{
 const doc=globalThis.document,options={capture:true,passive:false};
 for(const type of ['touchstart','touchmove'])doc?.addEventListener?.(type,e=>{if(e.touches.length>1)e.preventDefault();},options);

 for(const type of ['gesturestart','gesturechange'])doc?.addEventListener?.(type,e=>e.preventDefault(),options);
})();



globalThis.MkKeyboard=(()=>{
 const vv=globalThis.visualViewport,doc=globalThis.document,raf=globalThis.requestAnimationFrame?.bind(globalThis)||(fn=>setTimeout(fn,16)),listeners=new Set(),field=n=>!!n?.matches?.('input,textarea,[contenteditable]:not([contenteditable=false])');let inset=0,frame=0,resized=false;

 const occlusion=()=>!vv||Math.abs((vv.scale??1)-1)>.01?0:Math.max(0,Math.round(innerHeight-vv.height-vv.offsetTop));
 const measure=()=>{frame=0;let next=occlusion();

  if(next>0&&doc?.activeElement?.closest?.('.command-dock,.chat-composer,.sheet,.workspace-search,.gate-card')&&((globalThis.scrollY||0)>0||(vv.offsetTop||0)>0)){globalThis.scrollTo?.(0,0);next=occlusion();}
  const changed=next!==inset,notify=changed||resized;resized=false;if(changed){inset=next;doc?.documentElement?.style.setProperty('--keyboard-inset',`${inset}px`);}

  if(notify)for(const fn of listeners)fn(inset);};
 const schedule=e=>{if(e?.type==='resize'||e?.type==='orientationchange')resized=true;if(!frame)frame=raf(measure);};
 if(vv){vv.addEventListener('resize',schedule);vv.addEventListener('scroll',schedule);}for(const type of ['resize','orientationchange'])globalThis.addEventListener?.(type,schedule);for(const type of ['focusin','focusout'])doc?.addEventListener?.(type,schedule);
 return Object.freeze({inset:()=>inset,subscribe(fn){listeners.add(fn);return ()=>listeners.delete(fn);},

  keepFocus(container,signal){container?.addEventListener?.('mousedown',e=>{const b=e.target?.closest?.('button');if(b&&container.contains(b)&&field(doc?.activeElement)&&!field(e.target))e.preventDefault();},{capture:true,signal});}});
})();


globalThis.MkTerminalCodec=(()=>{
 const C=MkCrypto,decoder=new TextDecoder('utf-8',{fatal:true});
 const v=value=>{let n=BigInt(value),out=[];do{const b=Number(n&127n);n>>=7n;out.push(b|(n?128:0));}while(n);return Uint8Array.from(out);};
 const text=s=>{const b=C.enc.encode(s);return C.concat(v(b.length),b);};
 const frame=(kind,body=new Uint8Array())=>{const h=new Uint8Array(5);new DataView(h.buffer).setUint32(0,body.length+1,true);h[4]=kind;return C.concat(h,body);};
 function reader(bytes){let at=0;return {
  u(){let n=0n,shift=0n;for(let i=0;i<10;i++){if(at>=bytes.length)throw Error('Truncated screen');const b=bytes[at++];n|=BigInt(b&127)<<shift;if(!(b&128))return n;shift+=7n;}throw Error('Invalid screen integer');},
  n(max=Number.MAX_SAFE_INTEGER){if(at<bytes.length&&bytes[at]<128){const n=bytes[at++];if(n>max)throw Error('Screen limit');return n;}const n=Number(this.u());if(!Number.isSafeInteger(n)||n>max)throw Error('Screen limit');return n;},
  byte(){if(at>=bytes.length)throw Error('Truncated screen');return bytes[at++];},
  bool(){const n=this.byte();if(n>1)throw Error('Invalid boolean');return !!n;},
  text(max=4096){const n=this.n(max);if(at+n>bytes.length)throw Error('Truncated screen text');const s=n===0?'':n===1&&bytes[at]<128?String.fromCharCode(bytes[at]):decoder.decode(bytes.subarray(at,at+n));at+=n;return s;},
  end(){if(at!==bytes.length)throw Error('Trailing screen data');}
 };}
 function color(r){switch(r.n(2)){case 0:return null;case 1:return {index:r.byte()};case 2:return `rgb(${r.byte()},${r.byte()},${r.byte()})`;}}

 function cells(r){return Array.from({length:r.n(2048)},()=>{const ch=r.text(4),zero=Array.from({length:r.n(8)},()=>r.text(4)).join('');return {ch,zero,text:zero?(ch+zero).normalize('NFC'):ch,fg:color(r),bg:color(r),underline:color(r),flags:r.n(65535)};});}
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
 const SCALES=Object.freeze([.5,.75,1,1.25,1.5,2]),drafts=new Map(),modes=new Map(),instances=new Set(),uncertainInputs=new Set();
 const KEYS=Object.freeze([
  ['escape','Esc','\x1b'],['tab','Tab','\t'],['enter','Enter','\r'],['shiftTab','Shift+Tab','\x1b[Z'],['space','Space',' '],
  ['backspace','⌫','\x7f',true],['delete','Del','\x1b[3~',true],['arrowUp','↑','\x1b[A',true],['arrowDown','↓','\x1b[B',true],
  ['arrowLeft','←','\x1b[D',true],['arrowRight','→','\x1b[C',true],
  ['ctrlC','Ctrl+C','\x03'],['ctrlD','Ctrl+D','\x04'],
  ['ctrlL','Ctrl+L','\x0c'],['ctrlZ','Ctrl+Z','\x1a'],['ctrlR','Ctrl+R','\x12'],['ctrlA','Ctrl+A','\x01'],
  ['ctrlE','Ctrl+E','\x05'],['ctrlW','Ctrl+W','\x17'],['ctrlU','Ctrl+U','\x15'],
  ['home','Home','\x1b[H'],['end','End','\x1b[F'],['pageUp','PgUp','\x1b[5~',true],['pageDown','PgDn','\x1b[6~',true]
 ].map(([id,label,bytes,repeatable=false])=>Object.freeze({id,label,bytes,repeatable})));

 const DEFAULT_KEY_ORDER=Object.freeze(KEYS.map(k=>k.id));

 function encodeKey(bytes,mods=false){const {shift=false,alt=false,ctrl=false}=typeof mods==='boolean'?{shift:mods}:mods;if(!shift&&!alt&&!ctrl)return bytes;const n=1+(shift?1:0)+(alt?2:0)+(ctrl?4:0),letter=/^\x1b\[([ABCDHF])$/.exec(bytes),tilde=/^\x1b\[(\d+)~$/.exec(bytes);
  if(letter)return `\x1b[1;${n}${letter[1]}`;if(tilde)return `\x1b[${tilde[1]};${n}~`;if(bytes==='\t'&&shift)return (alt?'\x1b':'')+'\x1b[Z';if(bytes==='\r'&&shift)return `\x1b[13;${n}u`;if(bytes==='\x7f'&&ctrl)return (alt?'\x1b':'')+'\x08';return (alt?'\x1b':'')+bytes;}

 function controlChar(ch){if(ch===' '||ch==='@'||ch==='2')return '\x00';if(ch==='?')return '\x7f';const code=ch.toUpperCase().charCodeAt(0);return ch.length===1&&code>=64&&code<=95?String.fromCharCode(code-64):ch;}
 const icons={"monitor":[["rect",{"width":"20","height":"14","x":"2","y":"3","rx":"2"}],["line",{"x1":"8","x2":"16","y1":"21","y2":"21"}],["line",{"x1":"12","x2":"12","y1":"17","y2":"21"}]],"smartphone":[["rect",{"width":"14","height":"20","x":"5","y":"2","rx":"2","ry":"2"}],["path",{"d":"M12 18h.01"}]],"keyboard":[["path",{"d":"M10 8h.01"}],["path",{"d":"M12 12h.01"}],["path",{"d":"M14 8h.01"}],["path",{"d":"M16 12h.01"}],["path",{"d":"M18 8h.01"}],["path",{"d":"M6 8h.01"}],["path",{"d":"M7 16h10"}],["path",{"d":"M8 12h.01"}],["rect",{"width":"20","height":"16","x":"2","y":"4","rx":"2"}]],"dismiss":[["path",{"d":"M 20 4 A2 2 0 0 1 22 6"}],["path",{"d":"M 22 6 L 22 16.41"}],["path",{"d":"M 7 16 L 16 16"}],["path",{"d":"M 9.69 4 L 20 4"}],["path",{"d":"M14 8h.01"}],["path",{"d":"M18 8h.01"}],["path",{"d":"m2 2 20 20"}],["path",{"d":"M20 20H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2"}],["path",{"d":"M6 8h.01"}],["path",{"d":"M8 12h.01"}]],"send":[["path",{"d":"m5 12 7-7 7 7"}],["path",{"d":"M12 19V5"}]],"photo":[["rect",{"width":"18","height":"18","x":"3","y":"3","rx":"2","ry":"2"}],["circle",{"cx":"9","cy":"9","r":"2"}],["path",{"d":"m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"}]]};
 icons.edit=[['path',{d:'M10 5H3M12 19H3M14 3v4M16 17v4M21 12h-9M21 19h-5M21 5h-7M8 10v4M8 12H3'}]];
 icons.paste=[['path',{d:'M11 14h10'}],['path',{d:'M16 4h2a2 2 0 0 1 2 2v1.344'}],['path',{d:'m17 18 4-4-4-4'}],['path',{d:'M8 4H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 1.793-1.113'}],['rect',{x:'8',y:'2',width:'8',height:'4',rx:'1'}]];
 icons.zap=[['path',{d:'M4 14a1 1 0 0 1-.78-1.63l9.9-10.2a.5.5 0 0 1 .86.46l-1.9 6A1 1 0 0 0 14 10h6a1 1 0 0 1 .78 1.63l-9.9 10.2a.5.5 0 0 1-.86-.46l1.9-6A1 1 0 0 0 10 14z'}]];
 icons.direct=[['path',{d:'m6 17 5-5-5-5'}],['path',{d:'m13 17 5-5-5-5'}]];
 const make=(tag,cls,text)=>{const el=document.createElement(tag);if(cls)el.className=cls;if(text!==undefined)el.textContent=text;return el;};
 function icon(name){const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');for(const [k,v] of Object.entries({viewBox:'0 0 24 24',width:'16',height:'16',fill:'none',stroke:'currentColor','stroke-width':'2','stroke-linecap':'round','stroke-linejoin':'round','aria-hidden':'true'}))svg.setAttribute(k,v);for(const [tag,attrs] of icons[name]){const shape=document.createElementNS(svg.namespaceURI,tag);for(const [key,value] of Object.entries(attrs))shape.setAttribute(key,value);svg.append(shape);}return svg;}
 const translate=s=>globalThis.KrillMobile?.i18n?.t(s)||s;
 function button(cls,label,svg){label=translate(label);const el=make('button',cls,svg?undefined:label);el.type='button';el.setAttribute('aria-label',label);el.title=label;if(svg)el.append(icon(svg));return el;}
 function settings(){try{const value=JSON.parse(localStorage.getItem(UI_KEY)||'{}');return value&&typeof value==='object'&&!Array.isArray(value)?value:{};}catch{return {};}}
 const FAMILY='ui-monospace, "SF Mono", Menlo, Monaco, "Cascadia Mono", Consolas, "DejaVu Sans Mono", "Liberation Mono", "Symbols Nerd Font Mono", monospace';
 const PALETTE=['#15161e','#f7768e','#9ece6a','#e0af68','#7aa2f7','#bb9af7','#7dcfff','#a9b1d6','#414868','#f7768e','#9ece6a','#e0af68','#7aa2f7','#bb9af7','#7dcfff','#c0caf5'];
 const color=(c,fallback)=>{if(c==null)return fallback;if(typeof c==='string')return c;const n=c.index;if(n<16)return PALETTE[n];if(n>=232){const g=8+(n-232)*10;return `rgb(${g},${g},${g})`;}const k=n-16,levels=[0,95,135,175,215,255];return `rgb(${levels[Math.floor(k/36)]},${levels[Math.floor(k/6)%6]},${levels[k%6]})`;};

 const phoneLike=()=>!!globalThis.matchMedia?.('(pointer: coarse)')?.matches||(globalThis.innerWidth||Infinity)<600;
 const snap=value=>SCALES.reduce((best,n)=>Math.abs(n-value)<Math.abs(best-value)?n:best,1);


 function questionMenu(rows){
  const lines=rows.map(row=>row.filter(c=>!(c.flags&16)).map(c=>c.flags&256?' ':c.text).join('').replace(/^\s*[│┃]\s?/, '').replace(/\s*[│┃]\s*$/, '').trimEnd()),items=[];
  for(let y=0;y<lines.length;y++){
   const m=/^(\s*)([❯›>→])?\s*(?:(\d{1,2})[.)]\s+(\S.*)|(\[[ xX✓]\]|[☐☑])\s+(\S.*))$/.exec(lines[y]);if(!m)continue;
   let label=m[4]||m[6],box=m[5];const check=/^(\[[ xX✓]\]|[☐☑])\s+(\S.*)$/.exec(label);if(check){box=check[1];label=check[2];}
   items.push({label,number:m[3]?Number(m[3]):null,focused:!!m[2],checked:box?/[xX✓☑]/.test(box):null,line:y,indent:lines[y].indexOf(label)});
  }

  const first=items.findLastIndex(item=>item.number===1);if(first>0)items.splice(0,first);
  if(items.length<2||items.length>12||items.filter(i=>i.focused).length!==1)return null;
  const multiple=items.every(i=>i.checked!==null);if(!multiple&&items.some(i=>i.checked!==null))return null;
  if(items.some((item,i)=>item.number!==null&&item.number!==i+1))return null;
  const hint=/(?:enter|return|↵|엔터)/i,action=/(?:select|confirm|submit|continue|선택|확인|제출|계속)/i;
  const last=items.at(-1),after=lines.slice(last.line+1);while(after.length&&(!after[0].trim()||!hint.test(after[0])&&after[0].search(/\S/)>=last.indent)){const line=after.shift();if(line.trim())last.label+=' '+line.trim();}
  const tail=after.filter(line=>line.trim()&&!/^[\s─━╭╮╰╯┌┐└┘┴┬┼+\-]+$/.test(line));
  if(!tail.some(line=>hint.test(line)&&action.test(line)))return null;
  if(tail.some(line=>!/(?:enter|return|esc|tab|arrow|space|↑|↓|←|→|↵|엔터|선택|취소)/i.test(line)))return null;
  if(!multiple&&tail.some(line=>/space.*select|스페이스.*선택/i.test(line)))return null;
  for(let i=0;i<items.length-1;i++)for(const line of lines.slice(items[i].line+1,items[i+1].line)){
   if(!line.trim())continue;if(line.search(/\S/)<items[i].indent)return null;items[i].label+=' '+line.trim();
  }
  const active=items.findIndex(i=>i.focused),choices=items.map(({label,checked})=>({label,checked})),context=lines.slice(Math.max(0,items[0].line-4),items[0].line);
  return {choices,active,multiple,key:JSON.stringify([context,choices,active])};
 }
 class Terminal {
  constructor(root,options){
   if(!root||!Number.isSafeInteger(options.sessionId)||options.sessionId<=0||typeof options.connect!=='function')throw Error('Invalid terminal mount');
   this.root=root;this.options=options;this.session=options.sessionId;this.stateKey=options.stateKey??this.session;this.generation=1;this.inputGeneration=1;this.disposed=false;this.visible=options.visible!==false;
   this.abort=new AbortController();this.model={cols:80,lines:24,rows:[],cursor:{visible:false}};this.fit=false;this.ready=false;this.stick=true;this.autoFit=options.preferences?.fitToPhone??phoneLike();this.fontScale=snap(Number(options.preferences?.textScale)||1);
   this.dirty=new Set();this.full=true;this.inputId=0;this.historyId=0;this.pendingInputs=new Map();this.pendingRoles=[];this.deferredKeys=[];this.phase='connecting';this.held='';this.heldTimer=null;this.previewPending=false;this.alt=false;this.modeEpoch=0;this.root.dataset.alt='false';
   this.contextBlocked=uncertainInputs.has(this.stateKey);
   const stored=settings().terminalInputModes?.[this.stateKey],initial=options.preferences?.inputMode||modes.get(this.stateKey)||stored;
   this.inputMode=initial==='buffered'?'buffered':'direct';modes.set(this.stateKey,this.inputMode);
   this.build();this.resetHistory();this.bindInput();this.bindViewport();this.setFontScale(this.fontScale,false);this.updateMode();this.status('connecting','Connecting…');
   this.open();
  }
  listen(el,event,fn,extra={}){el.addEventListener(event,fn,{...extra,signal:this.abort.signal});}
  guard(action){const generation=this.generation;try{Promise.resolve(action()).catch(e=>{if(!this.disposed&&generation===this.generation)this.error(e);});}catch(e){if(generation===this.generation)this.error(e);}}
  status(phase,message,error){if(this.disposed)return;const waiting=phase==='ready'&&this.inputWaiting;message=translate(waiting?'Waiting for input confirmation…':message);this.phase=phase;this.root.dataset.phase=waiting?'input-pending':phase;this.updateLiveInput();this.syncQuestion();if(this.lastStatusPhase===phase&&this.lastStatusMessage===message&&!error)return;this.lastStatusPhase=error?null:phase;this.lastStatusMessage=error?null:message;this.statusEl.textContent=message;this.statusEl.setAttribute('role',error?'alert':'status');this.options.onStatus?.({phase,message,error,fit:this.fit,inputMode:this.inputMode,fontScale:this.fontScale,typing:!!this.wasTyping});}
  error(e){this.status('error',e.message||String(e),e);}
  build(){
   this.root.classList.add('terminal-mount');this.frame=make('div','terminal-frame');this.port=make('div','terminal-scroll');this.port.tabIndex=0;
   this.historyElement=make('pre','terminal-history');this.canvas=make('canvas','terminal-grid');this.canvas.setAttribute('role','img');

   this.textLayer=make('pre','terminal-text');this.textLayer.setAttribute('aria-hidden','true');this.ctx=this.canvas.getContext('2d');
   this.statusElement=this.statusEl=make('div','terminal-status');this.statusEl.dataset.testid='terminal-status';
   this.recoverButton=button('ghost small','Resume input');this.recoverButton.hidden=true;this.listen(this.recoverButton,'click',()=>{if(!this.ready||!this.isVisible())return;this.contextBlocked=false;uncertainInputs.delete(this.stateKey);this.recoverButton.hidden=true;this.status('ready',`${this.model.cols} × ${this.model.lines}`);this.focus();});

   const controls=this.historyControls=make('div','terminal-history-controls');this.historyButton=button('load-history','Load earlier output');this.historyStatus=make('span','history-status');this.historyStatus.setAttribute('role','status');controls.append(this.historyButton,this.historyStatus);this.port.append(controls,this.historyElement,this.canvas,this.textLayer);this.listen(globalThis.document||globalThis,'selectionchange',()=>{if(this.textStale||this.textRows?.size)this.syncText(new Set());});this.latestButton=button('terminal-latest','Latest output');this.latestButton.hidden=true;this.listen(this.latestButton,'pointerdown',e=>e.preventDefault());this.listen(this.latestButton,'click',()=>this.returnToLive());this.frame.append(this.port,this.latestButton,this.statusEl,this.recoverButton);
   this.dock=make('footer','command-dock');const accessory=this.accessory=make('div','accessory-row');this.dismiss=button('dismiss-keyboard','Dismiss keyboard','dismiss');this.dismiss.hidden=true;
   const keybar=make('div','keybar');keybar.setAttribute('role','toolbar');keybar.setAttribute('aria-label',translate('Terminal keys'));this.fitButton=button('accessory-key display-mode','Fit to Phone','monitor');this.fitButton.setAttribute('aria-pressed','false');this.directButton=button('accessory-key direct-toggle','Direct input','direct');this.shiftButton=button('accessory-key shift-toggle','Shift');this.shiftButton.dataset.keyId='shift';this.shiftButton.dataset.testid='terminal-key-shift';this.shiftButton.setAttribute('aria-pressed','false');

   this.modifierButtons={shift:this.shiftButton};for(const [name,label] of [['ctrl','Ctrl'],['alt','Alt']]){const b=this.modifierButtons[name]=button(`accessory-key ${name}-toggle`,label);b.dataset.keyId=name;b.dataset.testid=`terminal-key-${name}`;b.setAttribute('aria-pressed','false');this.listen(b,'pointerdown',e=>e.preventDefault());this.listen(b,'click',()=>this.toggleModifier(name));}

   this.pasteButton=button('accessory-key paste-key','Paste','paste');this.pasteButton.dataset.testid='terminal-paste';
   const ordered=this.options.preferences?.orderedKeyIds||[],visible=this.options.preferences?.visibleKeyIds;
   const ids=[...new Set([...ordered,...DEFAULT_KEY_ORDER])],keys=[];for(const id of ids){const key=KEYS.find(k=>k.id===id);if(!key||visible&&!visible.includes(id))continue;const b=button('accessory-key',key.label);b.dataset.keyId=id;b.dataset.testid=`terminal-key-${id}`;keys.push(b);this.bindKey(b,key);}

   keybar.append(this.fitButton,this.directButton,...keys,this.shiftButton,this.modifierButtons.ctrl,this.modifierButtons.alt);

   this.live=button('show-keyboard live-focus-target','Show keyboard','keyboard');const liveCopy=make('span','live-input-copy');this.liveTitle=make('strong',undefined,translate('Live input'));this.liveSubtitle=make('small',undefined,translate('Tap to show keyboard'));this.liveSubtitle.setAttribute('aria-live','polite');liveCopy.append(this.liveTitle,this.liveSubtitle);this.live.append(liveCopy);accessory.append(this.dismiss,keybar,this.pasteButton);const inputRow=this.inputRow=make('div','input-row');
   if(this.options.onQuickActions){const quick=button('accessory-key quick-actions','Quick actions','zap');quick.dataset.testid='terminal-quick-actions';quick.setAttribute('aria-haspopup','dialog');this.listen(quick,'pointerdown',e=>e.preventDefault());this.listen(quick,'click',()=>this.options.onQuickActions());accessory.append(quick);}
   this.capture=make('textarea','live-capture');this.capture.rows=1;this.capture.tabIndex=-1;this.capture.setAttribute('aria-label',translate('Terminal direct input'));this.capture.setAttribute('autocapitalize','none');this.capture.setAttribute('autocorrect','off');this.capture.setAttribute('autocomplete','off');this.capture.spellcheck=false;
   this.form=make('form','buffered-command');this.command=make('textarea','command-input');this.command.rows=1;this.command.placeholder=translate('Type a command…');this.command.setAttribute('aria-label',translate('Command'));this.command.setAttribute('enterkeyhint','enter');this.command.autocomplete='off';this.command.setAttribute('autocapitalize','none');this.command.spellcheck=false;this.command.value=drafts.get(this.stateKey)||'';
   this.sendButton=button('command-send','Send command','send');this.sendButton.type='submit';this.form.append(this.command,this.sendButton);inputRow.append(this.live,this.form);
   if(this.options.onPhoto){const photo=this.photo=button('photo-attach','Attach photo','photo'),images=make('input','photo-picker'),files=make('input','file-picker');images.type=files.type='file';images.accept=files.accept='image/*';images.setAttribute('aria-label',translate('Choose photo'));files.setAttribute('aria-label',translate('Choose image file'));images.hidden=files.hidden=true;accessory.append(photo);accessory.append(images,files);let held=false,timer;
    this.listen(photo,'pointerdown',()=>{held=false;timer=setTimeout(()=>{held=true;(globalThis.KrillMobile?.updates?.pickFile||((p)=>p.click()))(files);},350);});for(const event of ['pointerup','pointercancel','pointerleave'])this.listen(photo,event,()=>clearTimeout(timer));this.listen(photo,'click',()=>{if(!held)(globalThis.KrillMobile?.updates?.pickFile||((p)=>p.click()))(images);});this.listen(this.abort.signal,'abort',()=>clearTimeout(timer));
    for(const picker of [images,files])this.listen(picker,'change',()=>{const file=picker.files?.[0];if(file)this.guard(()=>this.options.onPhoto(file));picker.value='';});
   }
   if(this.options.onEditKeys){const edit=button('accessory-key edit-keys','Edit terminal keys','edit');keybar.append(edit);this.listen(edit,'click',()=>this.options.onEditKeys());}
   this.question=make('section','terminal-question');this.question.hidden=true;this.question.setAttribute('aria-label',translate('Choose an answer'));this.question.dataset.testid='terminal-question';
   inputRow.append(this.capture);this.dock.append(this.question,accessory,inputRow);this.root.append(this.frame,this.dock);
   let lastTop=0;this.listen(this.historyButton,'click',()=>this.guard(()=>this.loadHistory()));this.listen(this.port,'scroll',()=>{const top=this.port.scrollTop,up=top<lastTop&&!this.alt,refresh=up&&this.stick&&this.historyLoadedVersion!==this.historyVersion;if(top>lastTop&&this.nearWindowBottom()){lastTop=top;this.returnToLive();return;}const now=this.now(),elapsed=now-(this.historyScrollAt??now);this.historyVelocity=up&&elapsed>0?(lastTop-top)/elapsed:0;this.historyScrollAt=now;lastTop=top;this.stick=this.atBottom(1);this.latestButton.hidden=this.stick;this.syncQuestion();if(refresh||up&&top<this.historyThreshold())this.guard(()=>this.loadHistory(false,refresh));},{passive:true});
   for(const b of [this.fitButton,this.directButton,this.shiftButton,this.pasteButton,this.historyButton])this.listen(b,'pointerdown',e=>e.preventDefault());this.listen(this.pasteButton,'click',()=>this.pasteClipboard());this.listen(this.shiftButton,'click',()=>this.toggleShift());
   this.listen(this.fitButton,'click',()=>this.guard(()=>this.fitToPhone()));this.listen(this.directButton,'click',()=>this.setInputMode(this.inputMode==='direct'?'buffered':'direct'));
   this.listen(this.dismiss,'click',()=>{this.capture.blur();this.command.blur();});this.listen(this.live,'click',()=>this.focus());



   let pan;this.listen(this.port,'wheel',e=>{if(Math.abs(e.deltaY)>=Math.abs(e.deltaX)&&e.deltaY&&this.wheelRemote(e)){e.preventDefault();return;}if(e.deltaY>0&&this.nearWindowBottom()){this.returnToLive();return;}if(!this.alt&&e.deltaY<0&&(this.atBottom()&&this.historyLoadedVersion!==this.historyVersion||this.port.scrollTop<this.historyThreshold()))this.guard(()=>this.loadHistory(false,this.atBottom()&&this.historyLoadedVersion!==this.historyVersion));if(!this.fit&&Math.abs(e.deltaX)>Math.abs(e.deltaY)){e.preventDefault();this.port.scrollLeft+=e.deltaX;}},{passive:false});
   let pinch;const distance=touches=>Math.hypot((touches[0].clientX??touches[0].x??0)-(touches[1].clientX??touches[1].x??0),(touches[0].clientY??touches[0].y??0)-(touches[1].clientY??touches[1].y??0));
   const midpoint=touches=>{const r=this.port.getBoundingClientRect?.()||{left:0,top:0};return {x:((touches[0].clientX??touches[0].x??0)+(touches[1].clientX??touches[1].x??0))/2-(r.left||0),y:((touches[0].clientY??touches[0].y??0)+(touches[1].clientY??touches[1].y??0))/2-(r.top||0)};};
   this.listen(this.port,'touchstart',e=>{this.stopMomentum();const t=e.touches[0];pan=e.touches.length===1?{x:t.clientX,y:t.clientY,left:this.port.scrollLeft,axis:null,touch:true,live:this.atBottom(),tui:this.ownsLive(t),unscrollable:this.port.scrollHeight>0&&this.port.clientHeight>0&&this.port.scrollHeight<=this.port.clientHeight,lastY:t.clientY,rem:0,dir:0,samples:[{t:this.now(),x:t.clientX,y:t.clientY}]}:null;this.touching=!!pan;if(e.touches.length===2){const mid=midpoint(e.touches);pinch={distance:distance(e.touches),scale:this.fontScale,midX:mid.x,midY:mid.y};this.previewScale=this.fontScale;this.pinching=true;}},{passive:true});
   this.listen(this.port,'touchmove',e=>{if(pinch&&e.touches.length===2){e.preventDefault();const mid=midpoint(e.touches);pinch.midX=mid.x;pinch.midY=mid.y;this.previewScale=Math.max(.5,Math.min(2,pinch.scale*distance(e.touches)/pinch.distance));for(const el of [this.canvas,this.textLayer]){el.style.transformOrigin='top left';el.style.transform=`scale(${this.previewScale/this.fontScale})`;}return;}if(!pan||e.touches.length!==1)return;const p=e.touches[0],dx=p.clientX-pan.x,dy=p.clientY-pan.y,stepY=p.clientY-pan.lastY,now=this.now();pan.samples.push({t:now,x:p.clientX,y:p.clientY});pan.samples=pan.samples.filter(s=>now-s.t<=80);


    if(!pan.axis&&pan.tui&&this.remoteRoute()&&e.cancelable)e.preventDefault();

    if(!pan.axis&&Math.hypot(dx,dy)>6){pan.axis=this.lockAxis(dx,dy);if(pan.axis==='y'&&pan.tui){pan.route=this.claimRoute();pan.epoch=this.modeEpoch;}}if(pan.route&&pan.epoch!==this.modeEpoch)pan.axis='done';
    if(pan.axis==='x'){if(e.cancelable)e.preventDefault();this.port.scrollLeft=pan.left-dx;}else if(pan.axis==='y'&&pan.route){const d=p.clientY-pan.lastY;pan.lastY=p.clientY;pan.point=p;pan.reported=this.remoteScroll(d,p,pan)!==null;if(pan.reported&&e.cancelable)e.preventDefault();}else if(pan.axis==='y'){pan.lastY=p.clientY;if((pan.unscrollable&&!this.alt||this.alt&&this.fit&&this.historyElement.hidden)&&this.port.scrollHeight>this.port.clientHeight){if(e.cancelable)e.preventDefault();this.port.scrollTop=Math.max(0,Math.min(this.port.scrollHeight-this.port.clientHeight,(this.port.scrollTop||0)-stepY));}if(dy<-6&&!pan.pulled&&this.nearWindowBottom()){pan.pulled=true;this.returnToLive();}else if(dy>6&&!pan.pulled&&!this.alt&&(pan.live||this.port.scrollTop<this.historyThreshold())){pan.pulled=true;this.stick=false;this.latestButton.hidden=false;this.guard(()=>this.loadHistory(false,pan.live&&this.historyLoadedVersion!==this.historyVersion));}}},{passive:false});
   const finish=e=>{const g=pan;pan=null;this.touching=false;if(g&&e?.type==='touchend')this.fling(g);this.flushText();if(!pinch)return;const {scale:prevScale,midX,midY}=pinch,left=this.port.scrollLeft,top=this.port.scrollTop;pinch=null;this.canvas.style.transform=this.textLayer.style.transform='';this.setFontScale(this.previewScale||this.fontScale);const ratio=this.fontScale/prevScale;if(ratio!==1){this.port.scrollLeft=Math.max(0,(left+midX)*ratio-midX);this.port.scrollTop=Math.max(0,(top+midY)*ratio-midY);this.stick=false;}setTimeout(()=>{this.pinching=false;},0);};this.listen(this.port,'touchend',finish);this.listen(this.port,'touchcancel',finish);
  }
  questionEnabled(){return this.ready&&this.screenComplete&&!this.disposed&&!this.retired&&this.inputActive!==false&&this.isVisible()&&!this.contextBlocked&&!this.pasteReview&&!this.composing&&!this.bufferedComposing&&!this.pendingInputs.size&&!this.questionPending&&!this.historyWindowed&&this.stick;}
  syncQuestion(){
   if(!this.question)return;const menu=this.ready&&this.screenComplete?questionMenu(this.model.rows):null;this.liveQuestion=menu;
   if(!menu){if(this.screenComplete)this.questionSentKey=null;this.question.hidden=true;return;}
   const hidden=!this.isVisible()||this.inputActive===false||this.historyWindowed||!this.stick||this.contextBlocked||!this.ready,visibilityChanged=this.question.hidden!==hidden;this.question.hidden=hidden;
   const renderKey=JSON.stringify([menu.key,this.inputGeneration,this.modeEpoch]);
   const layoutChanged=visibilityChanged||renderKey!==this.questionRenderKey;
   if(renderKey!==this.questionRenderKey){this.questionRenderKey=renderKey;const generation=this.inputGeneration,epoch=this.modeEpoch,nodes=[make('small','question-label',translate(menu.multiple?'Select answers, then submit':'Choose an answer'))];
    const add=(label,index,checked)=>{const b=make('button','question-choice',label);b.type='button';b.setAttribute('aria-label',label);b.title=label;b.dataset.testid=index===null?'question-submit':`question-choice-${index+1}`;if(checked!==null){b.setAttribute('role','checkbox');b.setAttribute('aria-checked',String(checked));b.prepend(make('span','question-check',checked?'☑':'☐'));}b.addEventListener('pointerdown',e=>e.preventDefault());b.addEventListener('click',()=>this.guard(()=>this.answerQuestion(menu.key,index,generation,epoch)));nodes.push(b);};
    menu.choices.forEach((choice,index)=>add(choice.label,index,choice.checked));if(menu.multiple)add(translate('Submit answers'),null,null);this.question.replaceChildren(...nodes);
   }
   const disabled=!this.questionEnabled()||this.questionSentKey===menu.key;for(const b of this.question.children)if(b.tagName.toLowerCase()==='button')b.disabled=disabled;


   if(layoutChanged&&this.stick)this.port.scrollTop=this.port.scrollHeight;
  }
  async answerQuestion(key,index,generation,epoch){
   const current=this.screenComplete?questionMenu(this.model.rows):null;
   if(!this.questionEnabled()||generation!==this.inputGeneration||epoch!==this.modeEpoch||current?.key!==key||this.questionSentKey===key)return;
   if(index!==null&&(!Number.isInteger(index)||!current.choices[index]))return;
   this.questionSentKey=key;this.questionPending=true;this.syncQuestion();


   const delta=index===null?0:index-current.active,prefix=this.termModes?.flags&1?'\x1bO':'\x1b[',bytes=(prefix+(delta<0?'A':'B')).repeat(Math.abs(delta))+(current.multiple&&index!==null?' ':'\r');
   for(const name of ['shift','ctrl','alt'])this.toggleModifier(name,false);
   this.resetCapture();try{await this.input(bytes,{reveal:false});}finally{this.questionPending=false;this.syncQuestion();}
  }
  now(){return globalThis.performance?.now?.()??Date.now();}

  modeRoute(){const f=this.modesSupported?this.termModes?.flags||0:0;return (f&14)&&(f&16)?'sgr':!(f&14)&&(f&192)===192?'arrows':null;}
  remoteRoute(){return this.modifiers?.shift?null:this.modeRoute();}

  claimRoute(){if(this.modifiers?.shift&&this.modeRoute()){this.toggleModifier('shift',false);return null;}return this.remoteRoute();}


  canvasEdges(){const p=this.port,max=Math.max(0,(p.scrollHeight||0)-(p.clientHeight||0)),top=this.historyElement.hidden?0:this.canvas.offsetTop||0,raw=top+this.model.lines*(this.cellHeight||18)-(p.clientHeight||0),lo=Math.max(0,Math.min(top,raw,max));return {lo,hi:Math.max(lo,Math.min(Math.max(top,raw),max))};}

  ownsLive(point){if(this.canvas.hidden)return false;if(this.historyElement.hidden)return true;if((this.port.scrollTop||0)<this.canvasEdges().lo-1)return false;const rect=this.canvas.getBoundingClientRect?.();return !rect||!Number.isFinite(point?.clientY)||point.clientY>=rect.top;}
  nearWindowBottom(){const p=this.port;return !!this.historyWindowed&&!this.alt&&p.scrollHeight-p.scrollTop-p.clientHeight<(this.cellHeight||18)*2;}
  wheelStep(){return Math.max(8,(this.cellHeight||18)/1.25);}

  lockAxis(dx,dy){const p=this.port,wide=p.scrollWidth===undefined?!this.fit:p.scrollWidth>p.clientWidth+1;return wide&&Math.abs(dx)>1.25*Math.abs(dy)?'x':'y';}


  remoteScroll(d,point,g,max=100){if(!d)return true;const dir=Math.sign(d);if(dir!==g.dir){g.rem=0;g.dir=dir;}const {lo,hi}=this.canvasEdges(),s=this.port.scrollTop||0;if(dir>0?s>lo+.5:s<hi-.5){if(g.touch||this.alt&&this.fit&&this.historyElement.hidden){this.port.scrollTop=Math.max(lo,Math.min(hi,s-d));d-=s-this.port.scrollTop;if(Math.abs(d)<.5)return true;}else return null;}if(s<lo-.5)this.port.scrollTop=lo;
   const step=this.wheelStep();g.rem+=d;let n=Math.trunc(g.rem/step);g.rem-=n*step;if(Math.abs(n)>max){n=dir*max;g.rem=0;}return !n||this.report(this.wheelBytes(g.route,n,point));}
  wheelBytes(route,n,point){const k=Math.min(Math.abs(n),100),up=n>0;if(route==='arrows')return ((this.termModes?.flags&1?'\x1bO':'\x1b[')+(up?'A':'B')).repeat(k);const r=this.canvas.getBoundingClientRect?.()||{left:0,top:0},m=this.model,at=(v,max)=>Math.max(1,Math.min(max,Math.floor(v)+1));return `\x1b[<${up?64:65};${at(((point?.clientX??0)-r.left)/(this.cellWidth||8),m.cols)};${at(((point?.clientY??0)-r.top)/(this.cellHeight||18),m.lines)}M`.repeat(k);}

  wheelRemote(e){const now=this.now();let g=this.wheelGesture;if(!g||now>g.until)g=this.wheelGesture={route:this.ownsLive(e)?this.claimRoute():null,rem:0,dir:0,epoch:this.modeEpoch};g.until=now+200;if(!g.route||g.epoch!==this.modeEpoch)return false;const unit=e.deltaMode===1?this.cellHeight||18:e.deltaMode===2?this.port.clientHeight||0:1;return this.remoteScroll(-e.deltaY*unit,e,g)!==null;}

  report(text){if(this.disposed||this.retired||!this.ready||this.inputActive===false||!this.isVisible()||this.contextBlocked||this.pasteReview)return false;if(this.historyWindowed)this.returnToLive();const bytes=C.enc.encode(text);if(!bytes.length||bytes.length>65536)return false;try{this.send(0x13,C.concat(T.v(++this.inputId),T.v(bytes.length),bytes));}catch(e){this.error(e);return false;}return true;}

  fling(g){const axis=g.axis==='x'?'x':g.axis==='y'&&g.route&&g.reported?'y':null,s=g.samples,last=s?.at(-1);if(!axis||!last||this.now()-last.t>80)return;let i=s.length-1,dir=0;while(i>0){const d=s[i][axis]-s[i-1][axis],sg=Math.sign(d);if(sg&&dir&&sg!==dir)break;if(sg)dir=sg;i--;}const first=s[i],dt=last.t-first.t;if(!dir||!(dt>0))return;const v=Math.max(-6,Math.min(6,(last[axis]-first[axis])/dt));if(Math.abs(v)>=.25)this.startMomentum(axis,v,g);}
  startMomentum(axis,v,g){this.stopMomentum();const raf=globalThis.requestAnimationFrame||(fn=>setTimeout(fn,16)),start=this.now(),m=this.momentum={v,last:start,left:this.port.scrollLeft||0,id:0};
   const tick=()=>{if(this.momentum!==m||this.disposed)return;const now=this.now(),decay=Math.exp(-Math.min(50,Math.max(0,now-m.last))/320),delta=m.v*320*(1-decay);m.last=now;m.v*=decay;let ok;
    if(axis==='x'){m.left-=delta;this.port.scrollLeft=m.left;ok=Math.abs((this.port.scrollLeft||0)-m.left)<2;}else ok=this.remoteScroll(delta,g.point,g,8)===true;
    if(!ok||Math.abs(m.v)<.02||now-start>1800)this.stopMomentum();else m.id=raf(tick);};m.id=raf(tick);}
  stopMomentum(){const m=this.momentum;if(!m)return;this.momentum=null;(globalThis.cancelAnimationFrame||clearTimeout)(m.id);this.flushText();}

  applyModes(){const key=this.modesSupported?(this.termModes?.flags??0)&0xdf:-1;if(key===this.modeKey)return;this.modeKey=key;this.modeEpoch=(this.modeEpoch||0)+1;this.stopMomentum();this.wheelGesture=null;const alt=key>=0&&!!(key&64),was=this.alt;this.alt=alt;this.root.dataset.alt=String(alt);this.screenComplete=false;this.syncQuestion();if(alt===was)return;
   if(alt){clearTimeout(this.historyWarmTimer);this.historyWarmTimer=null;if(this.historyPending&&!this.historyPending.reveal){this.historyPending=null;this.historyReplacing=false;this.historyButton.disabled=this.historyEnd;this.historyStatus.textContent=translate(this.historyEnd?'Beginning of saved output':'Scroll up for earlier output');}this.historyElement.hidden=this.historyControls.hidden=true;this.canvas.hidden=this.textLayer.hidden=false;this.port.scrollTop=0;this.stick=true;this.latestButton.hidden=true;return;}

   this.historyElement.hidden=false;if(this.historyWindowed){this.returnToLive();return;}this.historyControls.hidden=!this.historyRows.length;this.canvas.hidden=this.textLayer.hidden=false;if(this.stick)this.scrollBottom();this.maybeLoadInitialHistory();}
  stopRepeat(){clearTimeout(this.repeatTimer);this.repeatToken=(this.repeatToken||0)+1;this.heldPointer=null;}
  bindKey(el,key){
   this.listen(el,'pointerdown',e=>{e.preventDefault();if(e.button!==0)return;this.stopRepeat();if(!key.repeatable)return;this.heldPointer=e.pointerId;el.setPointerCapture?.(e.pointerId);const token=this.repeatToken;
    const tick=async()=>{try{await this.sendKey(key.id);if(token===this.repeatToken&&!this.disposed)this.repeatTimer=setTimeout(tick,45);}catch(e){if(token===this.repeatToken){this.stopRepeat();this.error(e);}}};
    const initial=this.sendKey(key.id);initial.catch(e=>{if(token===this.repeatToken){this.stopRepeat();this.error(e);}});this.repeatTimer=setTimeout(()=>{initial.then(()=>{if(token===this.repeatToken&&!this.disposed)tick();},()=>{});},400);
   });
   this.listen(el,'click',e=>{if(!key.repeatable||e.detail===0)this.guard(()=>this.sendKey(key.id));});
   for(const event of ['pointerup','pointercancel','lostpointercapture','pointerleave'])this.listen(el,event,()=>this.stopRepeat());
  }
  bindInput(){


   this.listen(this.capture,'focus',()=>{this.resetCapture();this.inputArmed=this.inputActive!==false&&!this.contextBlocked&&!this.pasteReview&&this.isVisible();});
   this.listen(this.capture,'blur',()=>{if(this.inputArmed&&this.inputActive!==false&&!this.disposed&&this.inputMode==='direct')this.flushHeld();});
   this.listen(this.capture,'paste',e=>{if(!this.inputArmed||this.inputMode!=='direct'||this.composing)return;const text=e.clipboardData?.getData('text/plain');if(typeof text!=='string')return;e.preventDefault();this.flushHeld();const value=this.capture.value,start=this.capture.selectionStart??value.length,end=this.capture.selectionEnd??start;this.capture.value=value.slice(0,start)+text+value.slice(end);this.syncCapture(true);});
   this.listen(this.capture,'compositionstart',()=>{if(this.inputActive===false||this.inputMode!=='direct'||this.disposed)return;this.composing=true;this.compositionGeneration=this.inputGeneration;this.compositionConnection=this.generation;});
   this.listen(this.capture,'compositionend',e=>{
    clearTimeout(this.heldTimer);this.heldTimer=null;this.held='';
    const valid=this.inputArmed&&this.composing&&this.compositionGeneration===this.inputGeneration&&this.compositionConnection===this.generation&&this.inputMode==='direct';this.composing=false;
    if(!valid){const saved=this.inactiveComposition;this.inactiveComposition=null;if(saved&&saved.version===this.bufferedVersion){this.command.value=saved.prefix+(e.data||saved.text);this.command.value?drafts.set(this.stateKey,this.command.value):drafts.delete(this.stateKey);this.sizeCommand();}this.resetCapture();this.deferredKeys.splice(0).forEach(p=>p.reject(Error('Input target changed')));return;}
    this.syncCapture();const keys=this.deferredKeys.splice(0);if(keys.length)this.resetCapture();for(const pending of keys)this.input(pending.bytes).then(pending.resolve,pending.reject);
   });
   this.listen(this.capture,'input',e=>{if(this.disposed||!this.inputArmed||this.inputMode!=='direct'){this.resetCapture();return;}if(this.composing||e.isComposing){this.syncComposing();this.updateLiveInput();return;}clearTimeout(this.heldTimer);this.heldTimer=null;this.held='';this.syncCapture(e.inputType==='insertFromPaste');});

   this.listen(this.capture,'beforeinput',e=>{if(!this.inputArmed||this.inputMode!=='direct')return;if(this.held&&e.inputType==='deleteContentBackward'){e.preventDefault();clearTimeout(this.heldTimer);this.heldTimer=null;this.held='';this.capture.value=this.sent||'';this.updatePreview();return;}if(this.composing||e.isComposing)return;if(e.inputType==='deleteContentBackward'&&this.capture.value)return;const bytes={deleteContentBackward:'\x7f',deleteContentForward:'\x1b[3~',insertLineBreak:'\r',insertParagraph:'\r'}[e.inputType];if(bytes){e.preventDefault();this.guard(()=>this.controlInput(this.consumeShift(bytes,true)));}});
   this.listen(this.capture,'keydown',e=>{if(!this.inputArmed||this.inputMode!=='direct'||e.keyCode===229||e.metaKey)return;if(this.held&&e.key==='Backspace'&&!e.ctrlKey&&!e.altKey&&!this.modifiers?.ctrl&&!this.modifiers?.alt){e.preventDefault();clearTimeout(this.heldTimer);this.heldTimer=null;this.held='';this.capture.value=this.sent||'';this.updatePreview();return;}if(this.composing||e.isComposing)return;if(e.key==='Backspace'&&this.capture.value&&!e.ctrlKey&&!e.altKey&&!this.modifiers?.ctrl&&!this.modifiers?.alt)return;let bytes;const special={Escape:'escape',Tab:'tab',Enter:'enter',Backspace:'backspace',Delete:'delete',ArrowUp:'arrowUp',ArrowDown:'arrowDown',ArrowLeft:'arrowLeft',ArrowRight:'arrowRight',Home:'home',End:'end',PageUp:'pageUp',PageDown:'pageDown'}[e.key],mods={shift:!!(e.shiftKey||this.modifiers?.shift),ctrl:!!(e.ctrlKey||this.modifiers?.ctrl),alt:!!(e.altKey||this.modifiers?.alt)};if(special)bytes=encodeKey(KEYS.find(k=>k.id===special).bytes,mods);else if((mods.ctrl||mods.alt)&&[...e.key].length===1)bytes=(mods.alt?'\x1b':'')+(mods.ctrl?controlChar(e.key):e.key);if(bytes!==undefined){e.preventDefault();for(const name of ['shift','ctrl','alt'])if(this.modifiers?.[name])this.toggleModifier(name,false);this.guard(()=>this.controlInput(bytes));}});
   this.listen(this.command,'compositionstart',()=>{this.bufferedComposing=true;});this.listen(this.command,'compositionend',()=>{this.bufferedComposing=false;});

   this.listen(this.command,'input',()=>{this.bufferedVersion=(this.bufferedVersion||0)+1;this.command.value?drafts.set(this.stateKey,this.command.value):drafts.delete(this.stateKey);this.sizeCommand();});
   this.listen(this.form,'submit',e=>{e.preventDefault();if(this.inputActive===false||e.isComposing||this.bufferedComposing||this.bufferedPending)return;const value=this.command.value;if(!value)return;const generation=this.generation,version=this.bufferedVersion;this.bufferedPending=true;this.sendButton.disabled=true;
    const clean=value.replace(/\r\n?/g,'\n').replace(/[\x00-\x08\x0b-\x1f\x7f]/g,''),bytes=this.termModes?.flags&32?'\x1b[200~'+clean+'\x1b[201~':clean.replace(/\n/g,'\r');
    this.guard(async()=>{try{await this.input(bytes+'\r');if(!this.disposed&&generation===this.generation&&version===this.bufferedVersion&&this.command.value===value){this.command.value='';drafts.delete(this.stateKey);this.sizeCommand();}}finally{this.bufferedPending=false;this.sendButton.disabled=false;}});
   });
  }

  sizeCommand(){if(this.sizeFrame)return;this.sizeFrame=(globalThis.requestAnimationFrame||setTimeout)(()=>{this.sizeFrame=0;if(this.disposed)return;this.command.style.height='auto';this.command.style.height=`${this.command.scrollHeight||34}px`;});}
  updateMode(){const direct=this.inputMode==='direct';this.root.dataset.inputMode=this.inputMode;this.capture.hidden=!direct;this.live.hidden=!direct;this.form.hidden=direct;this.inputRow.hidden=false;if(this.photo){this.photo.remove();if(direct)this.inputRow.append(this.photo);else{this.sendButton.remove();this.form.append(this.photo,this.sendButton);}}this.directButton.title=translate(direct?'Direct input':'Buffered input');this.directButton.setAttribute('aria-label',this.directButton.title);this.directButton.setAttribute('aria-pressed',String(direct));this.updateLiveInput();if(!direct)this.sizeCommand();}
  setInputMode(mode){if(this.disposed||!['direct','buffered'].includes(mode)||mode===this.inputMode)return;const typing=[this.capture,this.command].includes(globalThis.document?.activeElement);this.invalidateInput();this.inputMode=mode;modes.set(this.stateKey,mode);const value=settings();value.terminalInputModes={...value.terminalInputModes,[this.stateKey]:mode};try{localStorage.setItem(UI_KEY,JSON.stringify(value));}catch{}this.options.onPreferenceChange?.({inputMode:mode,sessionId:this.session});this.updateMode();if(typing)this.focus();}
  getInputMode(){return this.inputMode;}

  updateLiveInput(){if(!this.liveSubtitle)return;const value=this.capture.value;this.liveSubtitle.textContent=value||translate(this.root.dataset.keyboardOpen==='true'?'Keystrokes send immediately':'Tap to show keyboard');this.liveSubtitle.dataset.preview=String(!!value);this.liveSubtitle.scrollLeft=value?this.liveSubtitle.scrollWidth||0:0;this.liveTitle.textContent=translate(this.inputWaiting?'Waiting for input confirmation…':'Live input');}
  updatePreview(){this.updateLiveInput();}
  resetCapture(){clearTimeout(this.heldTimer);this.heldTimer=null;this.held='';this.capture.value='';this.sent='';this.previewPending=false;this.updatePreview();}
  flushHeld(){clearTimeout(this.heldTimer);this.heldTimer=null;if(!this.held)return;const h=this.held;this.held='';this.sent=(this.sent||'')+h;this.sendCapture(h);this.updatePreview();}
  sendCapture(bytes){if(!bytes)return;if(!this.ready){this.invalidateInput();this.error(Error('Wait for the live terminal to connect'));return;}const generation=this.inputGeneration;this.previewPending=true;const p=this.input(bytes);this.updatePreview();this.guard(async()=>{try{await p;}catch(error){if(generation===this.inputGeneration){this.contextBlocked=true;this.invalidateInput();this.recoverButton.hidden=false;}throw error;}});}
  syncComposing(){const cur=this.capture.value;if(cur===(this.sent||'')){clearTimeout(this.heldTimer);this.heldTimer=null;this.held='';this.updatePreview();return;}if(/[\u1100-\u11ff\u3130-\u318f\uac00-\ud7a3]$/.test(cur)){const committed=cur.slice(0,-1),held=cur.slice(-1);if(committed!==(this.sent||'')){const before=[...this.sent||''],after=[...committed];let same=0;while(same<before.length&&same<after.length&&before[same]===after[same])same++;const added=after.slice(same).join(''),removed='\x7f'.repeat(before.length-same);this.sent=committed;const bytes=removed+(added?this.consumeShift(added,false):'');if(bytes)this.sendCapture(bytes);}this.held=held;clearTimeout(this.heldTimer);this.heldTimer=setTimeout(()=>this.flushHeld(),300);this.updatePreview();}}
  syncCapture(pasted=false){const before=[...this.sent||''],now=this.capture.value,after=[...now];let same=0;while(same<before.length&&same<after.length&&before[same]===after[same])same++;const added=after.slice(same).join(''),removed='\x7f'.repeat(before.length-same);if(pasted||/[\r\n]/.test(added)){this.resetCapture();this.paste(added,removed);return;}if(added&&(this.modifiers?.ctrl||this.modifiers?.alt)){this.resetCapture();const bytes=removed+this.consumeShift(added,false);this.guard(()=>this.input(bytes));return;}this.sent=now;const bytes=removed+(added?this.consumeShift(added,false):'');if(bytes)this.sendCapture(bytes);else this.updatePreview();}
  insertText(text){
   const clean=String(text??'').replace(/\r\n?/g,'\n').replace(/[\x00-\x08\x0b-\x1f\x7f]/g,'').replace(/^\n+|\n+$/g,'');if(!clean)return false;
   if(this.inputMode==='buffered'){
    const value=this.command.value,start=typeof this.command.selectionStart==='number'?this.command.selectionStart:value.length,end=typeof this.command.selectionEnd==='number'?this.command.selectionEnd:start;
    this.command.value=value.slice(0,start)+clean+value.slice(end);this.command.selectionStart=this.command.selectionEnd=start+clean.length;
    this.bufferedVersion=(this.bufferedVersion||0)+1;drafts.set(this.stateKey,this.command.value);this.sizeCommand();this.command.focus?.();
   }else{this.paste(clean);this.capture?.focus?.();}
   return true;
  }
  paste(text,prefix=''){

   const clean=text.replace(/\r\n?/g,'\n').replace(/[\x00-\x08\x0b-\x1f\x7f]/g,'');if(!clean&&!prefix)return;
   if(this.termModes&&this.termModes.flags&32){this.guard(()=>this.controlInput(prefix+'\x1b[200~'+clean+'\x1b[201~'));return;}
   if(!/[\n\t]/.test(clean)){this.guard(()=>this.controlInput(prefix+clean));return;}

   this.reviewPaste(clean,prefix);
  }
  reviewPaste(clean,prefix=''){
   this.cancelPaste();const generation=this.generation,inputGeneration=this.inputGeneration,review=this.pasteReview=make('form','buffered-command'),label=make('label',null,translate('Review paste — sending may run commands')),editor=make('textarea','command-input'),send=button('primary','Send paste'),cancel=button('ghost','Cancel');editor.value=clean;editor.setAttribute('aria-label',translate('Review multiline paste'));review.classList.add('paste-review');send.type='submit';label.append(editor);review.append(label,send,cancel);this.dock.append(review);this.capture.blur();this.stopRepeat();
   let composing=false;this.listen(editor,'compositionstart',()=>composing=true);this.listen(editor,'compositionend',()=>composing=false);this.listen(cancel,'click',()=>{this.cancelPaste();this.focus();});this.listen(review,'submit',e=>{e.preventDefault();if(composing||e.isComposing||send.disabled||review!==this.pasteReview)return;send.disabled=true;const value=editor.value.replace(/\r\n?/g,'\n').replace(/[\x00-\x08\x0b-\x1f\x7f]/g,'');if(!value&&!prefix||generation!==this.generation||inputGeneration!==this.inputGeneration||!this.isVisible()){this.cancelPaste();return;}const bytes=this.termModes&&this.termModes.flags&32?'\x1b[200~'+value+'\x1b[201~':value.replace(/\n/g,'\r');this.cancelPaste();this.guard(()=>this.controlInput(prefix+bytes));});editor.focus();
  }

  pasteClipboard(){if(this.disposed||this.inputActive===false||this.contextBlocked||this.pasteReview||!this.isVisible())return;const generation=this.generation,inputGeneration=this.inputGeneration,clipboard=globalThis.navigator?.clipboard;
   const manual=()=>{if(this.inputMode==='buffered')this.command.focus({preventScroll:true});else this.reviewPaste('');};if(typeof clipboard?.readText!=='function'){manual();return;}
   this.guard(async()=>{let text;try{text=await clipboard.readText();}catch{text=null;}if(this.disposed||generation!==this.generation||inputGeneration!==this.inputGeneration||this.pasteReview||!this.isVisible())return;if(text===null){manual();return;}if(!text)return;
    if(this.inputMode==='direct'){this.paste(text);return;}const value=this.command.value,start=this.command.selectionStart??value.length,end=this.command.selectionEnd??start;this.command.value=value.slice(0,start)+text+value.slice(end);this.command.selectionStart=this.command.selectionEnd=start+text.length;this.bufferedVersion=(this.bufferedVersion||0)+1;drafts.set(this.stateKey,this.command.value);this.sizeCommand();this.command.focus({preventScroll:true});});
  }
  cancelPaste(){this.pasteReview?.remove();this.pasteReview=null;}

  controlInput(bytes){this.flushHeld();this.resetCapture();return this.input(bytes);}
  focus(){if(!this.disposed&&this.inputActive!==false)(this.inputMode==='direct'?this.capture:this.command).focus({preventScroll:true});}

  setInputActive(active){active=!!active;if(this.inputActive===active)return;if(!active){

   clearTimeout(this.heldTimer);this.heldTimer=null;this.held='';this.inputActive=false;
   this.capture.blur();if(this.composing){const before=[...this.sent||''],after=[...this.capture.value];let same=0;while(same<before.length&&same<after.length&&before[same]===after[same])same++;const text=after.slice(same).join(''),prefix=this.command.value;this.bufferedVersion=(this.bufferedVersion||0)+1;this.inactiveComposition={prefix,text,version:this.bufferedVersion};this.command.value=prefix+text;this.command.value?drafts.set(this.stateKey,this.command.value):drafts.delete(this.stateKey);this.setInputMode('buffered');}else this.invalidateInput();this.bufferedComposing=false;
  }this.inputActive=active;this.syncQuestion();}
  invalidateInput(){this.inputGeneration++;this.inputArmed=false;this.composing=false;this.cancelPaste();this.resetCapture();this.capture.blur();this.command.blur();this.stopRepeat();this.stopMomentum();this.deferredKeys.splice(0).forEach(p=>p.reject(Error('Input target changed')));this.syncQuestion();}

  toggleModifier(name,force){if(name!=='shift')this.flushHeld();const mods=this.modifiers||={};mods[name]=typeof force==='boolean'?force:!mods[name];if(name==='shift')this.shiftArmed=mods.shift;this.modifierButtons?.[name]?.setAttribute('aria-pressed',String(!!mods[name]));return !!mods[name];}
  toggleShift(force){return this.toggleModifier('shift',force);}
  consumeShift(bytes,isKey=false){const mods={...this.modifiers};if(!mods.shift&&!mods.ctrl&&!mods.alt)return bytes;for(const name of ['shift','ctrl','alt'])if(mods[name])this.toggleModifier(name,false);if(isKey)return encodeKey(bytes,mods);const [first='',...rest]=[...bytes];let head=mods.shift?first.toUpperCase():first;if(mods.ctrl)head=controlChar(head);return (mods.alt?'\x1b':'')+head+rest.join('');}
  sendKey(id){const key=KEYS.find(k=>k.id===id);if(!key)return Promise.reject(Error('Unknown terminal key'));if(this.disposed)return Promise.reject(Error('Input target changed'));if(this.held){this.flushHeld();this.composing=false;}const bytes=this.consumeShift(key.bytes,true);if(this.composing)return new Promise((resolve,reject)=>this.deferredKeys.push({bytes,resolve,reject}));return this.controlInput(bytes);}
  bindViewport(){

   const viewport=inset=>{if(this.disposed)return;this.keyboardInset=inset;const open=inset>80;if(open&&this.root.dataset.keyboardOpen!=='true')(globalThis.requestAnimationFrame||setTimeout)(()=>this.revealCursor());this.root.dataset.keyboardOpen=String(open);this.dismiss.hidden=!open;this.updateLiveInput();this.queueFit();};
   this.listen(this.abort.signal,'abort',MkKeyboard.subscribe(viewport));MkKeyboard.keepFocus(this.dock,this.abort.signal);this.listen(globalThis,'pagehide',()=>this.disconnect());

   this.listen(globalThis.document||globalThis,'visibilitychange',()=>this.guard(()=>this.syncVisibility()));this.listen(globalThis,'blur',()=>this.stopRepeat());viewport(MkKeyboard.inset());
   if(globalThis.ResizeObserver){this.observer=new ResizeObserver(()=>{if(this.stick)this.scrollBottom();if(this.capture?.value)this.liveSubtitle.scrollLeft=this.liveSubtitle.scrollWidth||0;this.maybeLoadInitialHistory();this.queueFit();});this.observer.observe(this.port);}
  }
  setFontScale(scale,notify=true){if(this.disposed)return;this.fontScale=snap(Number(scale)||1);this.fontSize=Math.max(6,Math.round(13*this.fontScale));this.cellWidth=8*this.fontSize/13;this.cellHeight=18*this.fontSize/13;

   this.ctx.font=`300 ${this.fontSize}px ${FAMILY}`;this.font=null;const glyph=this.ctx.measureText?.('M')?.width,wide=this.ctx.measureText?.('한')?.width;
   for(const el of [this.historyElement,this.textLayer]){const st=el.style;st.fontSize=`${this.fontSize}px`;st.lineHeight=`${this.cellHeight}px`;st.fontFamily=FAMILY;st.fontWeight='300';if(glyph>0)st.letterSpacing=`${this.cellWidth-glyph}px`;if(wide>0)st.setProperty('--wide-spacing',`${2*this.cellWidth-wide}px`);}this.schedule(true);if(notify)this.options.onPreferenceChange?.({textScale:this.fontScale});this.queueFit();}

  isVisible(){return this.visible&&globalThis.document?.visibilityState!=='hidden';}
  setVisible(visible){if(this.disposed)return Promise.resolve();this.visible=!!visible;return this.syncVisibility();}
  async syncVisibility(){const visible=this.isVisible(),version=this.visibilityVersion=(this.visibilityVersion||0)+1;clearTimeout(this.fitTimer);if(!visible){this.stopMomentum();this.invalidateInput();clearTimeout(this.labelTimer);this.labelTimer=null;if(this.rafId)(globalThis.cancelAnimationFrame||clearTimeout)(this.rafId);this.rafId=0;this.full=true;}else{this.schedule(true);if(this.stick)this.scrollBottom();this.maybeLoadInitialHistory();}if(!this.ready)return;if(this.resizePending)await this.resizePending.catch(()=>{});if(this.disposed||!this.ready||version!==this.visibilityVersion)return;if(!visible){await this.role(Uint8Array.from([0,2,0]));return;}if(this.autoFit&&!this.fit)await this.fitToPhone(true,false);else if(this.fit)await this.resize(true);this.maybeLoadInitialHistory();}
  queueFit(){clearTimeout(this.fitTimer);if(!this.isVisible()||!this.fit||!this.ready||this.fitPending)return;this.fitTimer=setTimeout(()=>{if(this.resizePending)return this.queueFit();if(this.isVisible()&&this.fit&&this.ready&&!this.fitPending&&!this.disposed)this.guard(()=>this.resize());},150);}

  atBottom(slack=this.cellHeight||18){const p=this.port;return (!this.historyWindowed||this.alt)&&p.scrollHeight-p.scrollTop-p.clientHeight<slack;}
  returnToLive(){if(this.historyWindowed){this.resetHistory();this.guard(()=>this.loadHistory());}this.scrollBottom();}
  scrollBottom(){this.port.scrollTop=this.port.scrollHeight;this.stick=true;this.latestButton.hidden=true;this.syncQuestion();}
  async open(){
   const generation=this.generation;let result;try{result=await this.options.connect({sessionId:this.session});}catch(e){if(generation===this.generation)this.disconnect('disconnected',e.message);return;}
   const {socket,credentials,channel:warmChannel,messages}=result||{};if(this.disposed||generation!==this.generation){socket?.close();return;}if(!socket||!credentials){this.error(Error('Invalid terminal connection factory'));return;}
   if(socket.readyState===2||socket.readyState===3||socket.closed){this.disconnect('disconnected','Disconnected');return;}
   this.socket=socket;this.connectionTimer=setTimeout(()=>{if(generation===this.generation)this.disconnect('disconnected','Terminal connection timed out');},15000);socket.binaryType='arraybuffer';let phase=warmChannel?1:0,channel=warmChannel;

   const onMessage=e=>{if(generation!==this.generation||this.disposed)return;try{
    if(phase===0){const challenge=JSON.parse(e.data),nonce=crypto.getRandomValues(new Uint8Array(32));if(challenge.version!==2)throw Error('Unsupported terminal protocol');channel=new C.Channel(C.sha256(C.unhex(credentials.credential)),C.concat(C.unhex(challenge.challenge),nonce));this.channel=channel;socket.send(JSON.stringify({device_id:credentials.device_id,token:null,client_nonce:C.hex(nonce)}));socket.send(channel.seal(C.enc.encode('{"label":"Live terminal"}')));

     this.send(2,C.concat(T.v(1),T.v(1),T.v((1n<<56n)|1n|4n|4096n|8192n),T.v(0),T.text('krill-mobile-grid')));this.guard(()=>this.role(Uint8Array.from([0,2,0])));this.send(0x10,C.concat(T.v(this.session),Uint8Array.from([0,0])));this.prefit();phase=1;return;}
    const bytes=channel.open(e.data);if(phase===1){JSON.parse(new TextDecoder().decode(bytes));phase=2;return;}
    if(phase===2){if(bytes[4]!==1)throw Error('Unexpected terminal handshake');const hello=T.reader(bytes.slice(5));hello.n();hello.n();this.modesSupported=!!(hello.u()&8192n);phase=3;return;}
    if(bytes.length<5||new DataView(bytes.buffer,bytes.byteOffset).getUint32(0,true)!==bytes.length-4)throw Error('Invalid screen frame');const body=bytes.slice(5),r=T.reader(body);
    switch(bytes[4]){
     case 0x20:this.attached={seq:r.u(),revision:r.u()};break;
     case 0x21:{try{this.screenComplete=false;const result=T.update(body,this.model);if(!result.done)this.syncQuestion();for(const y of result.dirty)this.dirty.add(y);if(result.done){this.screenComplete=true;this.historyVersion=(this.historyVersion||0)+1;clearTimeout(this.connectionTimer);const first=!this.ready;this.ready=true;this.previewPending=false;this.updatePreview();this.maybeLoadInitialHistory();if(first&&this.autoFit&&!this.fit&&this.isVisible())this.guard(()=>this.fitToPhone(true,false));this.schedule();if(uncertainInputs.has(this.stateKey)){this.recoverButton.hidden=false;this.status('ready','Input delivery is uncertain; input will not be resent.');}else if(!this.contextBlocked)this.status('ready',`${this.model.cols} × ${this.model.lines}`);if(this.model.title!==this.title){this.title=this.model.title;this.options.onTitle?.(this.title||'');}this.syncQuestion();}}catch(error){this.send(0x11);throw error;}break;}
     case 0x26:this.receiveHistory(T.history(body));break;
     case 0x27:{const id=r.n(),result=r.n(2),pending=this.pendingInputs.get(id);if(!pending)break;this.pendingInputs.delete(id);clearTimeout(pending.timer);clearTimeout(pending.slowTimer);if(result){this.inputWaiting=false;const error=Error('Terminal input rejected. Resume input, then re-enter rejected text; it will not be resent.');this.contextBlocked=true;this.invalidateInput();this.recoverButton.hidden=false;pending.reject(error);this.error(error);}else{pending.resolve();if(!this.pendingInputs.size){this.inputWaiting=false;this.previewPending=false;this.updatePreview();if(!this.contextBlocked)this.status('ready',`${this.model.cols} × ${this.model.lines}`);}}break;}
     case 0x2b:{if(!this.modesSupported)break;this.termModes={kitty:r.byte(),flags:r.n(65535)};r.end();this.applyModes();break;}
     case 0x2c:{const result=r.n(3),pending=this.pendingRoles.shift();if(pending){clearTimeout(pending.timer);if(result)pending.reject(Error('Terminal resize rejected'));else pending.resolve();}else if(result)throw Error('Terminal role rejected');break;}
     case 0x24:this.disconnect('ended','Session ended');break;
     case 0x25:this.disconnect('error','Session unavailable');break;
    }
   }catch(error){this.error(error);if(phase<3)socket.close();}};
   this.listen(socket,'message',onMessage);
   this.listen(socket,'close',e=>{if(generation===this.generation)this.disconnect(e.code===1008?'denied':'disconnected',e.code===1008?'Terminal authentication rejected':'Disconnected');});this.listen(socket,'error',()=>{if(generation===this.generation)this.disconnect('disconnected','Live terminal connection failed');});
   try{
    if(warmChannel){this.channel=warmChannel;this.guard(()=>this.role(Uint8Array.from([0,2,0])));this.send(0x10,C.concat(T.v(this.session),Uint8Array.from([0,0])));this.prefit();}
    if(Array.isArray(messages))for(const data of messages.splice(0))onMessage({data});
   }catch(error){this.error(error);if(phase<3)socket.close();}
  }
  send(kind,body){if(!this.socket||this.socket.readyState!==1||!this.channel)throw Error('Terminal disconnected');this.socket.send(this.channel.seal(T.frame(kind,body)));}

  revealCursor(){const c=this.model.cursor,p=this.port;if(this.disposed||!c?.visible)return;const top=(this.canvas.offsetTop||0)+c.line*this.cellHeight,bottom=top+this.cellHeight;if(top<p.scrollTop)p.scrollTop=top;else if(bottom>p.scrollTop+p.clientHeight)p.scrollTop=bottom-p.clientHeight;}
  input(text,{reveal=true}={}){if(this.disposed||this.retired||!this.ready)return Promise.reject(Error('Wait for the live terminal to connect'));if(this.inputActive===false||!this.isVisible()||this.contextBlocked||this.pasteReview)return Promise.reject(Error('Resume terminal input before sending'));if(this.historyWindowed)this.returnToLive();if(reveal)this.revealCursor();const bytes=C.enc.encode(text),id=++this.inputId;if(bytes.length>65536)return Promise.reject(Error('Input is too large (maximum 64 KiB)'));return new Promise((resolve,reject)=>{const pending={resolve,reject,slowTimer:setTimeout(()=>{if(this.pendingInputs.has(id)&&!this.contextBlocked){this.inputWaiting=true;this.status('ready','Waiting for input confirmation…');}},1000),timer:setTimeout(()=>{if(this.pendingInputs.has(id))this.disconnect('disconnected','Input confirmation timed out');},10000)};this.pendingInputs.set(id,pending);if(this.liveQuestion)this.questionSentKey=this.liveQuestion.key;this.syncQuestion();try{this.send(0x13,C.concat(T.v(id),T.v(bytes.length),bytes));}catch(e){this.pendingInputs.delete(id);clearTimeout(pending.timer);clearTimeout(pending.slowTimer);this.contextBlocked=true;this.invalidateInput();this.recoverButton.hidden=false;reject(e);}});}
  role(body){return new Promise((resolve,reject)=>{const pending={resolve,reject,timer:setTimeout(()=>{this.socket?.close();this.disconnect('disconnected','Terminal resize timed out');},5000)};this.pendingRoles.push(pending);try{this.send(0x17,body);}catch(e){this.pendingRoles.pop();clearTimeout(pending.timer);reject(e);}});}
  async fitToPhone(enabled=!this.fit,save=true){if(!this.ready)throw Error('Wait for the live terminal to connect');if(this.fitPending||enabled===this.fit)return;const generation=this.generation;this.fitPending=true;this.fitButton.disabled=true;try{if(enabled)await this.resize(true);else await this.role(Uint8Array.from([0,2,0]));if(this.disposed||generation!==this.generation)return;this.fit=enabled;this.root.dataset.fit=String(enabled);this.fitButton.setAttribute('aria-pressed',String(enabled));this.fitButton.replaceChildren(icon(enabled?'smartphone':'monitor'));if(enabled)this.port.scrollLeft=0;if(save){this.autoFit=enabled;this.options.onPreferenceChange?.({fitToPhone:enabled});}if(!this.contextBlocked)this.status('ready',`${this.model.cols} × ${this.model.lines}`);}finally{this.fitPending=false;this.fitButton.disabled=false;this.queueFit();}}

  fitSize(){const p=this.port;if(!p.clientWidth)return null;const keyboard=this.root.dataset.keyboardOpen==='true',rowsFor=n=>Math.max(10,Math.min(200,Math.floor(n/this.cellHeight))),cols=Math.max(10,Math.min(2048,Math.floor((p.clientWidth-2)/this.cellWidth))),rows=!p.clientHeight?this.model.lines:keyboard?this.fitRows??rowsFor(p.clientHeight+(this.keyboardInset||0)):(this.fitRows=rowsFor(p.clientHeight));return {cols,rows};}
  async resize(force=false){if(!this.isVisible())return;const size=this.fitSize();if(!size)return;const {cols,rows}=size;if(this.resizePending)return this.resizePending;if(!force&&cols===this.model.cols&&rows===this.model.lines)return;this.resizePending=this.role(C.concat(T.v(1),T.v(cols),T.v(rows)));try{await this.resizePending;}finally{this.resizePending=null;}}

  prefit(){if(!this.isVisible()||!this.autoFit||this.fit||this.resizePending||!this.port.clientHeight)return;const size=this.fitSize();if(!size)return;const pending=this.role(C.concat(T.v(1),T.v(size.cols),T.v(size.rows))),clear=()=>{if(this.resizePending===pending)this.resizePending=null;};pending.then(clear,clear);this.resizePending=pending;}
  historyThreshold(){return Math.max(this.cellHeight*6,(this.port.clientHeight||0)*3,Math.min((this.port.clientHeight||this.cellHeight)*6,(this.historyVelocity||0)*Math.max(250,(this.historyRtt||0)*2)));}
  maybeLoadInitialHistory(){if(this.historyNeedsInitialLoad&&this.ready&&this.isVisible()&&this.port.clientHeight&&!this.alt&&!this.historyPending)this.guard(()=>this.loadHistory());}
  warmHistory(){if(this.alt||this.historyWarmTimer||!this.isVisible()||!this.port.clientHeight||!this.stick||this.historyEnd||this.historyWindowed||this.historyRows.length>=900)return;this.historyWarmTimer=setTimeout(()=>{this.historyWarmTimer=null;if(this.stick&&!this.alt&&this.isVisible()&&!this.historyWindowed&&this.historyRows.length<900)this.guard(()=>this.loadHistory(false,false,Math.min(300,900-this.historyRows.length)));},0);}
  resetHistory(){clearTimeout(this.historyWarmTimer);this.historyWarmTimer=null;this.historySource=null;this.historySkip=0;this.historyTotal=null;this.historyHotTotal=null;this.historyPending=null;this.historyQueued=null;this.historyNeedsInitialLoad=true;this.historyRows=[];this.historyEnd=false;this.historyWindowed=false;this.historyReplacing=false;this.historyLoadedVersion=-1;this.canvas.hidden=this.textLayer.hidden=false;this.historyElement.textContent='';this.historyElement.hidden=!!this.alt;this.historyControls.hidden=true;}


  loadHistory(reveal=false,refresh=false,count=300){

   if(!this.ready||this.disposed||this.alt&&reveal!==true)return;
   if(this.historyPending){if(reveal===true||refresh)this.historyQueued={reveal:!!(this.historyQueued?.reveal||reveal===true),refresh:!!(this.historyQueued?.refresh||refresh)};return;}
   refresh=refresh||this.historyEnd&&!this.historyRows.length&&this.historyLoadedVersion!==(this.historyVersion||0);
   if(this.historyEnd&&!refresh)return;
   this.historyNeedsInitialLoad=false;

   this.historyReveal=reveal===true;if(this.historyReveal||this.historyRows.length)this.historyControls.hidden=false;

   if(refresh){this.historyReplacing=true;count=Math.min(1000,Math.max(count,this.historyRows.length));}const req=++this.historyId,source=refresh?null:this.historySource,skip=refresh?0:this.historySkip;
   this.historyPending={req,source,skip,refresh,reveal:this.historyReveal,replace:this.historyReplacing,started:this.now(),version:this.historyVersion||0};this.historyButton.disabled=true;this.historyStatus.textContent=translate('Loading earlier output…');
   this.send(0x12,C.concat(T.v(req),source===null?T.v(0):C.concat(T.v(1),T.v(source)),T.v(skip),T.v(count)));
  }


  historyRow(row,styled=true){const out=[];let run='',cur=null;const flush=()=>{if(!run)return;if(!cur.w&&!cur.fg&&!cur.bg&&!cur.st)out.push(run);else{const s=make('span',null,run),st=cur.st;if(cur.w)s.style.letterSpacing='var(--wide-spacing)';if(cur.fg)s.style.color=cur.fg;if(cur.bg)s.style.backgroundColor=cur.bg;if(st&1)s.style.fontWeight='500';if(st&2)s.style.fontStyle='italic';if(st&128)s.style.opacity='.65';if(st&96){s.style.textDecoration=`${st&32?'underline ':''}${st&64?'line-through':''}`.trim();if(cur.ul)s.style.textDecorationColor=cur.ul;}out.push(s);}run='';};for(const c of row){if(c.flags&16)continue;const f=c.flags,w=!!(f&8)&&!(f&256),st=!styled||f&256?0:f&227,fg=!styled||f&256?null:f&4?color(c.bg,'#1a1b26'):color(c.fg,null),bg=!styled?null:f&4?color(c.fg,'#c0caf5'):color(c.bg,null),ul=st&32?color(c.underline,null):null,k=`${w?1:0}|${fg||''}|${bg||''}|${ul||''}|${st}`;if(!cur||cur.k!==k){flush();cur={k,w,fg,bg,ul,st};}run+=f&256?' ':c.text;}flush();return out;}
  receiveHistory(reply){
   const pending=this.historyPending;if(!pending||reply.req!==pending.req)return;

   if(this.alt&&!pending.reveal){this.historyPending=null;this.historyReplacing=false;return;}
   const prev={source:this.historySource,skip:this.historySkip,end:this.historyEnd,total:this.historyTotal};this.historyPending=null;this.historyRtt=this.now()-pending.started;this.historyLoadedVersion=pending.version;if(pending.source===null&&!reply.skip)this.historyTotal=reply.total;this.historySkip=reply.skip+reply.rows.length;
   if(this.historySkip>=reply.total||!reply.rows.length){this.historySource=pending.source===null?reply.pages-1:pending.source-1;this.historySkip=0;this.historyEnd=this.historySource<0;}else{this.historySource=pending.source;this.historyEnd=false;}

   if(!reply.rows.length&&!this.historyEnd)return this.loadHistory(this.historyReveal);


   let rows=reply.rows;
   if(pending.source===null){
    if(!pending.replace&&pending.skip>0&&this.historyRows.length){
     const incoming=rows.map(row=>JSON.stringify(row)),existing=this.historyRows.slice(0,rows.length).map(row=>JSON.stringify(row)),growth=Math.max(0,reply.total-(this.historyHotTotal??reply.total));
     for(let overlap=Math.min(rows.length,existing.length);overlap>0;overlap--){const start=rows.length-overlap;if(incoming[start]!==existing[0]||!existing.slice(0,overlap).every((row,index)=>row===incoming[start+index]))continue;if(growth>=overlap||new Set(existing.slice(0,overlap)).size>1){rows=rows.slice(0,start);break;}}
    }
    this.historyHotTotal=reply.total;
   }
   this.historyReplacing=false;
   const reveal=this.historyReveal,following=this.stick;
   if(reveal&&this.alt)this.historyElement.hidden=false;
   if(!pending.replace||reveal||!this.mergeRefresh({...reply,rows},following,prev)){
    const before=this.canvas.offsetTop??this.port.scrollHeight,anchor=this.historyElement.children[0]?.getBoundingClientRect?.().top,oldRows=pending.replace?[]:this.historyRows,combined=[...rows,...oldRows];


    this.historyWindowed=(!pending.replace&&this.historyWindowed)||combined.length>1000;this.historyRows=combined.slice(0,1000);this.historyElement.hidden=this.alt&&!reveal;this.historyControls.hidden=this.alt&&!reveal||!this.historyRows.length;this.canvas.hidden=this.textLayer.hidden=this.historyWindowed;if(this.historyWindowed)this.stick=false;
    const added=rows.map(row=>{const line=make('span','terminal-history-row');line.append(...this.historyRow(row),'\n');return line;});

    if(pending.replace)this.historyElement.replaceChildren(...added);else this.historyElement.prepend(...added);
    while(this.historyElement.children.length>1000)this.historyElement.children[this.historyElement.children.length-1].remove();
    if(following&&!this.historyReveal&&!this.historyWindowed)this.scrollBottom();else{const moved=!pending.replace&&anchor!=null?this.historyElement.children[rows.length]?.getBoundingClientRect?.().top-anchor:null,delta=moved??(this.historyWindowed?rows.length*this.cellHeight:(this.canvas.offsetTop??this.port.scrollHeight)-before);if(delta)this.port.scrollTop+=delta;}
   }
   this.historyButton.disabled=this.historyEnd;this.historyStatus.textContent=translate(this.historyEnd?'Beginning of saved output':'Scroll up for earlier output');
   if(this.historyReveal){this.historyReveal=false;this.port.scrollTop=0;this.stick=false;}
   this.latestButton.hidden=this.stick&&!this.historyWindowed;


   const queued=this.historyQueued;this.historyQueued=null;
   if(queued&&(queued.reveal||queued.refresh&&this.historyLoadedVersion!==(this.historyVersion||0)))this.guard(()=>this.loadHistory(queued.reveal,queued.refresh&&this.historyLoadedVersion!==(this.historyVersion||0)));
   else if(!reveal&&!this.stick&&!this.historyEnd&&this.port.scrollTop<this.historyThreshold())this.guard(()=>this.loadHistory());else this.warmHistory();
  }


  mergeRefresh(reply,following,prev){const old=this.historyRows,fresh=reply.rows,el=this.historyElement;if(!old.length||!fresh.length||this.historyWindowed||el.children.length!==old.length)return false;
   const key=row=>row.map(c=>c.flags&16?'':c.flags&256?' ':c.flags&8?c.text+'\u0001':c.text).join(''),a=old.map(key),b=fresh.map(key),K=Math.min(8,a.length),maxJ=prev.total!=null&&reply.total>=prev.total?b.length-(reply.total-prev.total):b.length;let ov=-1;
   for(let j=Math.min(b.length,maxJ);j>=K&&ov<0;j--){let i=1;while(i<=K&&b[j-i]===a[a.length-i])i++;if(i<=K)continue;const m=Math.min(a.length,j);while(i<=m&&b[j-i]===a[a.length-i])i++;if(i>m)ov=j;}
   if(ov<0)return false;const added=fresh.slice(ov),missing=Math.max(0,ov-a.length),extra=Math.max(0,a.length-ov),drop=Math.max(0,a.length+missing+added.length-1000),keepsOld=extra>drop;

   if(keepsOld&&drop&&prev.source!==null)return false;
   if(keepsOld){this.historySource=prev.source;this.historySkip=prev.source===null?prev.skip+added.length-drop:prev.skip;this.historyEnd=prev.end;}
   if(!missing&&!added.length&&!drop)return true;
   const h=this.cellHeight||18,mark=el.children[drop],a0=mark?.offsetTop,before=this.canvas.offsetTop,top=this.port.scrollTop||0,live=before==null||top+(this.port.clientHeight||0)>before-1;
   const line=row=>{const span=make('span','terminal-history-row');span.append(...this.historyRow(row),'\n');return span;};
   for(let i=0;i<drop;i++)el.children[0].remove();if(missing)el.prepend(...fresh.slice(0,missing).map(line));if(added.length)el.append(...added.map(line));
   this.historyRows=[...fresh.slice(0,missing),...old.slice(drop),...added];this.historyControls.hidden=false;
   if(following)this.scrollBottom();else{const after=this.canvas.offsetTop,delta=live?(after!=null&&before!=null?after-before:(missing-drop+added.length)*h):mark?.offsetTop!=null&&a0!=null?mark.offsetTop-a0:(missing-drop)*h;if(delta)this.port.scrollTop=top+delta;}
   return true;}

  schedule(full=false){if(full)this.full=true;if(this.rafId||this.disposed||!this.isVisible())return;const raf=globalThis.requestAnimationFrame||(fn=>setTimeout(fn,16));this.rafId=raf(()=>{this.rafId=0;if(!this.disposed)this.draw();});}
  draw(){
   if(this.disposed||!this.isVisible()){this.full=true;return;}
   const m=this.model,ctx=this.ctx,w=this.cellWidth||8,h=this.cellHeight||18,font=this.fontSize||13,cssW=m.cols*w,cssH=m.lines*h;

   const scale=Math.min(globalThis.devicePixelRatio||1,Math.sqrt(16e6/(cssW*cssH))),width=Math.round(cssW*scale),height=Math.round(cssH*scale);
   let full=this.full||this.canvas.width!==width||this.canvas.height!==height;this.full=false;
   if(this.canvas.width!==width||this.canvas.height!==height){this.canvas.width=width;this.canvas.height=height;this.font=null;}
   if(this.cssSize!==`${cssW}x${cssH}`){this.cssSize=`${cssW}x${cssH}`;this.canvas.style.width=this.textLayer.style.width=`${cssW}px`;this.canvas.style.height=this.textLayer.style.height=`${cssH}px`;this.textLayer.style.marginTop=`${-cssH}px`;if(this.stick)this.scrollBottom();}
   ctx.setTransform(scale,0,0,scale,0,0);ctx.textBaseline='top';
   const cursor=m.cursor?.visible?{line:m.cursor.line,column:m.cursor.column,shape:m.cursorStyle?.shape??2}:null,cursorKey=JSON.stringify(cursor),dirty=this.dirty;this.dirty=new Set();
   if(cursorKey!==this.cursorKey){if(this.cursorLine!=null)dirty.add(this.cursorLine);if(cursor)dirty.add(cursor.line);}this.cursorKey=cursorKey;this.cursorLine=cursor?.line;
   ctx.fillStyle='#1a1b26';if(full)ctx.fillRect(0,0,cssW,cssH);else for(const y of dirty)if(y<m.lines)ctx.fillRect(0,y*h,cssW,h);
   for(let y=0;y<m.lines;y++){if(!full&&!dirty.has(y))continue;let runText='',runX=0,runLen=0,runFg='',runFlags=0,runUl='',runKey='';const flushRun=()=>{if(!runText)return;ctx.fillStyle=runFg;ctx.globalAlpha=runFlags&128?.65:1;const face=`${runFlags&2?'italic ':''}${runFlags&1?'500':'300'} ${font}px ${FAMILY}`;if(face!==this.font){ctx.font=face;this.font=face;}ctx.fillText(runText,runX*w,y*h+font/13);if(runFlags&32){ctx.fillStyle=runUl;ctx.fillRect(runX*w,y*h+h-3*font/13,runLen*w,font/13);}if(runFlags&64)ctx.fillRect(runX*w,y*h+h/2,runLen*w,font/13);ctx.globalAlpha=1;runText='';};for(let x=0;x<(m.rows[y]?.length||0);x++){const c=m.rows[y][x],f=c.flags;if(f&16)continue;const width=f&8?w*2:w;let fg=color(c.fg,'#c0caf5'),bg=color(c.bg,'#1a1b26');if(f&4)[fg,bg]=[bg,fg];if(bg!=='#1a1b26'){ctx.fillStyle=bg;ctx.fillRect(x*w,y*h,width,h);}if(f&256){flushRun();continue;}if(f&8){flushRun();ctx.fillStyle=fg;ctx.globalAlpha=f&128?.65:1;const face=`${f&2?'italic ':''}${f&1?'500':'300'} ${font}px ${FAMILY}`;if(face!==this.font){ctx.font=face;this.font=face;}ctx.fillText(c.text,x*w,y*h+font/13,width);if(f&32){ctx.fillStyle=color(c.underline,fg);ctx.fillRect(x*w,y*h+h-3*font/13,width,font/13);}if(f&64)ctx.fillRect(x*w,y*h+h/2,width,font/13);ctx.globalAlpha=1;continue;}const sf=f&(1|2|4|32|64|128),ul=f&32?color(c.underline,fg):'',key=`${fg}\0${bg}\0${sf}\0${ul}`;if(runText&&x===runX+runLen&&key===runKey){runText+=c.text;runLen++;}else{flushRun();runText=c.text;runX=x;runLen=1;runFg=fg;runFlags=sf;runUl=ul;runKey=key;}}flushRun();}
   if(cursor&&(full||dirty.has(cursor.line))){ctx.fillStyle='#c0caf5';const x=cursor.column*w,y=cursor.line*h;if(cursor.shape===2)ctx.fillRect(x,y,Math.max(1,font/13),h);else if(cursor.shape===1)ctx.fillRect(x,y+h-2,w,2);else{ctx.strokeStyle='#c0caf5';ctx.strokeRect(x+.5,y+.5,w-1,h-1);}}
   this.syncText(full?null:dirty);

   this.labelTimer??=setTimeout(()=>{this.labelTimer=null;if(!this.disposed&&this.isVisible())this.canvas.setAttribute('aria-label',this.screenText());},500);
  }

  syncText(rows){const layer=this.textLayer,m=this.model;if(!rows||layer.children.length!==m.lines)this.textStale=true;else for(const y of rows)if(y<m.lines)(this.textRows||=new Set()).add(y);if(this.touching||this.momentum)return;const s=globalThis.getSelection?.();if(s&&!s.isCollapsed&&(s.containsNode?.(layer,true)||s.rangeCount&&s.getRangeAt?.(0)?.intersectsNode?.(layer)))return;
   const line=y=>{const el=make('span','terminal-text-row');el.append(...this.historyRow(m.rows[y]||[],false),'\n');return el;};if(this.textStale)layer.replaceChildren(...Array.from({length:m.lines},(_,y)=>line(y)));else for(const y of this.textRows||[])layer.children[y]?.replaceWith(line(y));this.textStale=false;this.textRows=null;}
  flushText(){if(this.textStale||this.textRows?.size)this.syncText(new Set());}
  screenText(){return this.model.rows.map(row=>row.filter(c=>!(c.flags&16)).map(c=>(c.flags&256)?' ':c.text).join('')).join('\n');}
  text(){return (this.historyElement.textContent+this.screenText()).replace(/[ \t]+$/gm,'').replace(/\n+$/,'');}
  disconnect(phase='disconnected',message='Disconnected'){
   clearTimeout(this.connectionTimer);clearTimeout(this.fitTimer);clearTimeout(this.historyWarmTimer);this.historyWarmTimer=null;this.generation++;this.ready=false;this.inputWaiting=false;this.fit=false;this.termModes=null;this.modesSupported=false;this.applyModes();this.recoverButton.hidden=true;delete this.root.dataset.fit;this.wasTyping=[this.capture,this.command].includes(globalThis.document?.activeElement);this.invalidateInput();const uncertain=this.pendingInputs.size,error=Error('Input delivery is uncertain; input will not be resent.');if(uncertain){uncertainInputs.add(this.stateKey);this.contextBlocked=true;}for(const pending of this.pendingInputs.values()){clearTimeout(pending.timer);clearTimeout(pending.slowTimer);pending.reject(error);}this.pendingInputs.clear();for(const pending of this.pendingRoles.splice(0)){clearTimeout(pending.timer);pending.reject(Error('Terminal disconnected'));}this.channel=null;this.socket?.close();this.fitButton.setAttribute('aria-pressed','false');this.fitButton.replaceChildren(icon('monitor'));this.status(phase,uncertain?error.message:message,uncertain?error:undefined);
  }
  unmount(){if(this.disposed)return;instances.delete(this);this.command.value?drafts.set(this.stateKey,this.command.value):drafts.delete(this.stateKey);this.disconnect();this.disposed=true;this.abort.abort();this.observer?.disconnect();clearTimeout(this.labelTimer);if(this.rafId)(globalThis.cancelAnimationFrame||clearTimeout)(this.rafId);this.frame.remove();this.dock.remove();this.root.classList.remove('terminal-mount');this.root.style.removeProperty('--keyboard-inset');delete this.root.dataset.keyboardOpen;delete this.root.dataset.inputMode;delete this.root.dataset.phase;delete this.root.dataset.fit;delete this.root.dataset.alt;}
 }
 const mobile=globalThis.KrillMobile||(globalThis.KrillMobile={});
 mobile.terminal=Object.freeze({keys:KEYS,defaultKeyOrder:DEFAULT_KEY_ORDER,fontScales:SCALES,
  pruneSessions(liveSessionsMap,scopePrefix=''){
   const alive=key=>{const text=String(key);if(!text.startsWith(scopePrefix))return true;const id=text.slice(scopePrefix.length),session=liveSessionsMap.get(Number(id))||liveSessionsMap.get(id);return !!session&&!session.exited&&session.state!=='Exited';};
   for(const i of instances)if(!alive(i.stateKey)&&!i.retired){i.retired=true;i.disconnect('ended','Session ended');i.contextBlocked=false;}
   for(const key of uncertainInputs)if(!alive(key))uncertainInputs.delete(key);
  },
  updateBusy(scope=''){return [...uncertainInputs].some(key=>String(key).startsWith(scope))||[...instances].some(i=>String(i.stateKey).startsWith(scope)&&!i.retired&&(i.composing||i.bufferedComposing||i.bufferedPending||i.pendingInputs.size||i.pendingRoles.length||i.deferredKeys.length||i.heldPointer!=null||i.pasteReview||i.contextBlocked||i.sent||i.held||i.historyRows.length&&!i.atBottom()));},
  snapshotDrafts(){const saved=new Map(drafts);for(const i of instances){if(i.command.value)saved.set(i.stateKey,i.command.value);else saved.delete(i.stateKey);}return [...saved];},
  restoreDrafts(entries){for(const [key,value] of entries)if(typeof key==='string'&&typeof value==='string'&&value)drafts.set(key,value);},
  mount(root,options){const instance=new Terminal(root,options);instances.add(instance);return Object.freeze(Object.fromEntries(['unmount','focus','setInputActive','setVisible','setFontScale','setInputMode','getInputMode','insertText','sendKey','toggleShift','fitToPhone','loadHistory','text'].map(name=>[name,instance[name].bind(instance)])));}});
})();
