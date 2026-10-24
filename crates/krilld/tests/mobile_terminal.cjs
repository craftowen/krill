// Standalone terminal adapter regression harness: node crates/krilld/tests/mobile_terminal.cjs
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/terminal.js'),'utf8');
class Element extends EventTarget {
 constructor(tag){super();this.tagName=tag;this.children=[];this.dataset={};this.attrs={};this.style={setProperty(k,v){this[k]=v;},removeProperty(k){delete this[k];}};this.classList={add(){},remove(){}};this.value='';this.clientWidth=390;this.scrollTop=0;this.scrollHeight=0;this.namespaceURI='http://www.w3.org/2000/svg';}
 get textContent(){return this.children.length?this.children.map(c=>typeof c==='string'?c:c.textContent).join(''):this.text??'';}
 set textContent(v){this.children=[];this.text=v;}
 append(...nodes){this.children.push(...nodes);for(const n of nodes)if(typeof n==='object')n.parent=this;}
 prepend(...nodes){this.children.unshift(...nodes);for(const n of nodes)if(typeof n==='object')n.parent=this;}
 replaceChildren(...nodes){this.children=[];this.append(...nodes);}
 setAttribute(k,v){this.attrs[k]=v;}
 getAttribute(k){return this.attrs[k];}
 remove(){if(this.parent)this.parent.children=this.parent.children.filter(n=>n!==this);}
 focus(){this.dispatchEvent(new Event('focus'));}
 blur(){this.dispatchEvent(new Event('blur'));}
 getContext(){const calls=this.calls={fillText:0,font:0,draws:0};return new Proxy({},{get:(_,k)=>k==='fillText'?t=>{calls.fillText++;calls.texts?.push(t);}:k==='setTransform'?()=>{calls.draws++;}:()=>{},set:(_,k)=>{if(k==='font')calls.font++;return true;}});}
 setPointerCapture(){}
}
function fixture(){
 globalThis.MkCrypto={enc:new TextEncoder(),concat:(...arrays)=>{const out=new Uint8Array(arrays.reduce((n,a)=>n+a.length,0));let at=0;for(const a of arrays){out.set(a,at);at+=a.length;}return out;},unhex:()=>new Uint8Array(32),hex:()=>'',sha256:a=>a,Channel:class{seal(a){return a;}open(a){return new Uint8Array(a);}}};
 globalThis.sockets=[];globalThis.FakeSocket=class extends EventTarget {
  constructor(){super();this.readyState=1;this.sent=[];sockets.push(this);}
  send(data){this.sent.push(data);if(this.autoAck&&data instanceof Uint8Array&&data[4]===0x13){const r=MkTerminalCodec.reader(data.slice(5)),id=r.n();queueMicrotask(()=>this.frame(0x27,MkTerminalCodec.v(id),MkTerminalCodec.v(0)));}}
  message(data){const e=new Event('message');e.data=data;this.dispatchEvent(e);}
  frame(kind,...bytes){this.message(MkTerminalCodec.frame(kind,MkCrypto.concat(...bytes)));}
  close(){if(this.readyState===3)return;this.readyState=3;this.dispatchEvent(new Event('close'));}
  handshake(features=(1n<<56n)|8192n|4096n|5n){const T=MkTerminalCodec;this.message(JSON.stringify({version:2,challenge:''}));this.message(new TextEncoder().encode('{}'));this.frame(1,T.v(1),T.v(1),T.v(features));this.frame(0x2c,T.v(0));}
  screen(text='Krill · live terminal'){const T=MkTerminalCodec,C=MkCrypto,chars=[...text],cell=ch=>C.concat(T.text(ch),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0));this.frame(0x21,T.v(1),T.v(1),T.v(1),T.v(4),T.v(0),T.v(80),T.v(24),T.v(2),T.v(0),T.v(chars.length),...chars.map(cell),T.v(3),T.v(1),T.v(2),T.v(1),T.v(4),T.text('Terminal #7'));}
 };
 globalThis.mountFixture=(root,sessionId=7,extra={})=>KrillMobile.terminal.mount(root,{sessionId,connect:()=>({socket:new FakeSocket(),credentials:{device_id:'fixture',credential:''}}),...extra});
}
const viewport=Object.assign(new EventTarget(),{scale:1,height:844,offsetTop:0});
const globalEvents=new EventTarget(),storage=new Map(),timers=new Map();let clock=0,nextTimer=0;
const context=vm.createContext({Event,EventTarget,AbortController,TextEncoder,TextDecoder,Uint8Array,DataView,Map,Set,Promise,console,queueMicrotask,crypto:require('node:crypto').webcrypto,innerHeight:844,devicePixelRatio:1,visualViewport:viewport,document:Object.assign(new EventTarget(),{visibilityState:'visible',documentElement:new Element('html'),createElement:tag=>new Element(tag),createElementNS:(_,tag)=>new Element(tag)}),localStorage:{getItem:k=>storage.get(k),setItem:(k,v)=>storage.set(k,v)},addEventListener:globalEvents.addEventListener.bind(globalEvents),setTimeout:(fn,delay)=>{const id=++nextTimer;timers.set(id,{fn,at:clock+delay});return id;},clearTimeout:id=>timers.delete(id)});
vm.runInContext(`(${fixture})()`,context);vm.runInContext(source,context);
const api=context.KrillMobile.terminal,T=context.MkTerminalCodec,C=context.MkCrypto;
const event=(el,type,props={})=>{const e=new Event(type,{cancelable:true});Object.assign(e,props);el.dispatchEvent(e);return e;};
const find=(root,cls)=>{if(root.className?.split(' ').includes(cls))return root;for(const c of root.children||[]){const found=find(c,cls);if(found)return found;}};
const key=(root,id)=>find(root,'keybar').children.find(el=>el.dataset.keyId===id);
const flush=async()=>{for(let i=0;i<8;i++)await Promise.resolve();};
async function advance(ms){const end=clock+ms;for(;;){const next=[...timers].filter(([,t])=>t.at<=end).sort((a,b)=>a[1].at-b[1].at)[0];if(!next)break;clock=next[1].at;timers.delete(next[0]);next[1].fn();await flush();}clock=end;await flush();}
const inputs=socket=>Array.from(socket.sent).filter(a=>a instanceof Uint8Array&&a[4]===0x13).map(a=>{const r=T.reader(a.slice(5));r.n();return r.text();});
async function mount(id=7,extra={}){const root=new Element('div'),handle=context.mountFixture(root,id,extra);await flush();const socket=context.sockets.at(-1);socket.handshake();socket.screen();await flush();return {root,handle,socket};}
async function testHiddenPaint(){
 const hidden=await mount(1901,{visible:false,preferences:{fitToPhone:false}});
 // Locate the actual canvas without relying on the wrapper's class.
 function getCanvas(node){if(node.tagName==='canvas')return node;for(const child of node.children||[]){const c=getCanvas(child);if(c)return c;}}
 const paint=getCanvas(hidden.root);for(let n=0;n<10;n++){hidden.socket.screen('hidden '+n);await advance(16);}await advance(500);assert.equal(paint.calls.draws,0);assert.equal(paint.getAttribute('aria-label'),undefined);assert.match(hidden.handle.text(),/hidden 9/,'hidden wire frames still decode into current model');
 await hidden.handle.setVisible(true);await advance(16);assert.equal(paint.calls.draws,1);await advance(500);assert.match(paint.getAttribute('aria-label'),/hidden 9/);context.document.visibilityState='hidden';context.document.dispatchEvent(new Event('visibilitychange'));await flush();const draws=paint.calls.draws,label=paint.getAttribute('aria-label');hidden.socket.screen('background current');await advance(600);assert.equal(paint.calls.draws,draws);assert.equal(paint.getAttribute('aria-label'),label);context.document.visibilityState='visible';context.document.dispatchEvent(new Event('visibilitychange'));await advance(16);assert.equal(paint.calls.draws,draws+1);assert.match(hidden.handle.text(),/background current/);hidden.handle.unmount();
 console.log('PASS hidden canvas draws/aria 0, model decode retained, restore full paint once (view and document visibility)');
}
async function main(){
 await testHiddenPaint();
 for(const n of [0,1,127,128,300,65535,Number.MAX_SAFE_INTEGER]){const r=T.reader(T.v(n));assert.equal(r.n(),n);r.end();}assert.throws(()=>T.reader(T.v(128)).n(127),/Screen limit/);assert.throws(()=>T.reader(T.v(128)).n(128-1),/Screen limit/);assert.throws(()=>T.reader(T.v(1n<<53n)).n(),/Screen limit/);assert.throws(()=>T.reader(Uint8Array.of(128)).n(),/Truncated/);
 for(const text of ['', 'a', '\0', '한글', 'é', '😀']){const r=T.reader(T.text(text));assert.equal(r.text(),text);r.end();}assert.throws(()=>T.reader(Uint8Array.of(1,255)).text());assert.throws(()=>T.reader(Uint8Array.of(2,97)).text(),/Truncated/);
 assert(!source.includes('innerHTML'));assert(!source.includes('getElementById'));assert.equal(api.keys.length,24);
 const expected=['\x1b','\t','\r','\x1b[Z',' ','\x7f','\x1b[3~','\x1b[A','\x1b[B','\x1b[D','\x1b[C','\x1b[H','\x1b[F','\x1b[5~','\x1b[6~','\x03','\x04','\x0c','\x1a','\x12','\x01','\x05','\x17','\x15'];
 let {root,handle,socket}=await mount();socket.autoAck=true;
 for(const k of api.keys)await handle.sendKey(k.id);assert.deepEqual(inputs(socket),expected);console.log('PASS exact 24-key order and bytes');
 // Switching visible panes invalidates a late IME commit, deferred keys, repeat and paste review.
 const inactiveCapture=find(root,'live-capture');handle.focus();event(inactiveCapture,'compositionstart');inactiveCapture.value='late';const deferred=handle.sendKey('enter');const rejected=assert.rejects(deferred,/Input target changed/);const inactiveCount=inputs(socket).length;handle.setInputActive(false);await rejected;assert.equal(find(root,'command-input').value,'late');event(inactiveCapture,'compositionend',{data:'final'});assert.equal(find(root,'command-input').value,'final');inactiveCapture.value='late';event(inactiveCapture,'input');event(inactiveCapture,'paste',{clipboardData:{getData:()=> 'wrong-pane'}});await assert.rejects(handle.sendKey('escape'),/Resume/);await flush();assert.equal(inputs(socket).length,inactiveCount);assert.equal(inactiveCapture.value,'');
 handle.setInputActive(true);handle.setInputMode('direct');handle.focus();inactiveCapture.value='active';event(inactiveCapture,'input');await flush();assert.equal(inputs(socket).at(-1),'active');console.log('PASS inactive pane discards late IME, deferred keys, paste and direct input');
 assert.equal(handle.toggleShift(),true);assert.equal(key(root,'shift').getAttribute('aria-pressed'),'true');await handle.sendKey('tab');assert.equal(inputs(socket).at(-1),'\x1b[Z');assert.equal(key(root,'shift').getAttribute('aria-pressed'),'false');
 event(key(root,'shift'),'click');await handle.sendKey('arrowUp');assert.equal(inputs(socket).at(-1),'\x1b[1;2A');
 event(key(root,'ctrl'),'click');assert.equal(key(root,'ctrl').getAttribute('aria-pressed'),'true');inactiveCapture.value='c';event(inactiveCapture,'input');await flush();assert.equal(inputs(socket).at(-1),'\x03');assert.equal(key(root,'ctrl').getAttribute('aria-pressed'),'false');assert.equal(inactiveCapture.value,'');
 inactiveCapture.value='x';event(inactiveCapture,'input');await flush();assert.equal(inputs(socket).at(-1),'x');
 event(key(root,'alt'),'click');inactiveCapture.value='xb';event(inactiveCapture,'input');await flush();assert.equal(inputs(socket).at(-1),'\x1bb');
 event(key(root,'ctrl'),'click');await handle.sendKey('arrowRight');assert.equal(inputs(socket).at(-1),'\x1b[1;5C');event(key(root,'alt'),'click');await handle.sendKey('arrowLeft');assert.equal(inputs(socket).at(-1),'\x1b[1;3D');
 event(key(root,'ctrl'),'click');event(key(root,'shift'),'click');await handle.sendKey('pageUp');assert.equal(inputs(socket).at(-1),'\x1b[5;6~');event(key(root,'alt'),'click');await handle.sendKey('backspace');assert.equal(inputs(socket).at(-1),'\x1b\x7f');
 console.log('PASS Shift, Ctrl and Alt one-shot modifiers');
 {const html=context.document.documentElement,writes=[],set=html.style.setProperty;html.style.setProperty=(k,v)=>{writes.push([k,v]);set.call(html.style,k,v);};
  viewport.height=700;event(viewport,'resize');viewport.height=544;event(viewport,'resize');event(viewport,'scroll');assert.deepEqual(writes,[],'keyboard events only schedule; no layout write before the frame');await advance(16);assert.deepEqual(writes,[['--keyboard-inset','300px']],'one CSS variable write per frame on <html>');assert.equal(root.style['--keyboard-inset'],undefined,'terminal does not keep its own inset');assert.equal(find(root,'dismiss-keyboard').hidden,false);assert.equal(root.dataset.keyboardOpen,'true');
  viewport.scale=1.5;event(viewport,'resize');await advance(16);assert.deepEqual(writes.at(-1),['--keyboard-inset','0px'],'pinch zoom is not a keyboard');assert.equal(find(root,'dismiss-keyboard').hidden,true);viewport.scale=undefined;event(viewport,'resize');await advance(16);assert.deepEqual(writes.at(-1),['--keyboard-inset','300px'],'a WebView without scale is unzoomed');
  const scrolls=[];context.scrollY=40;context.scrollTo=(x,y)=>{scrolls.push([x,y]);context.scrollY=0;};context.document.activeElement={closest:s=>s.includes('.command-dock')?{}:null};viewport.offsetTop=40;viewport.height=504;event(viewport,'scroll');await advance(16);assert.deepEqual(scrolls,[[0,0]],'iOS focus scroll is put back for the docked input');viewport.offsetTop=0;viewport.height=544;event(viewport,'scroll');await advance(16);assert.equal(scrolls.length,1);assert.equal(writes.length,3,'unchanged inset is not rewritten');
  context.document.activeElement=null;viewport.scale=1;viewport.height=844;event(viewport,'resize');await advance(16);assert.deepEqual(writes.at(-1),['--keyboard-inset','0px']);html.style.setProperty=set;}
 {const prevented=[],container={addEventListener(type,fn,opts){assert.equal(type,'mousedown');assert.equal(opts.capture,true);this.fn=fn;},contains:()=>true},button={},tap=target=>{const e={target,preventDefault(){prevented.push(target);}};container.fn(e);};context.MkKeyboard.keepFocus(container);const onButton={closest:s=>s==='button'?button:null,matches:()=>false},onText={closest:()=>null,matches:()=>true};
  context.document.activeElement={matches:()=>true};tap(onButton);tap(onText);context.document.activeElement={matches:()=>false};tap(onButton);context.document.activeElement=null;assert.deepEqual(prevented,[onButton],'a button tap keeps the text field focused only while typing');}
 console.log('PASS visualViewport keyboard inset: one rAF-batched <html> write, pinch zoom ignored, iOS focus scroll reset, send taps keep the keyboard');
 const attach=socket.sent.find(a=>a[4]===0x10),role=socket.sent.find(a=>a[4]===0x17);assert.deepEqual([...attach.slice(-2)],[0,0]);assert.deepEqual([...role.slice(5)],[0,2,0]);
 let count=inputs(socket).length;event(key(root,'backspace'),'pointerdown',{button:0,pointerId:1});await flush();assert.equal(inputs(socket).length,count+1);await advance(399);assert.equal(inputs(socket).length,count+1);await advance(1);assert.equal(inputs(socket).length,count+2);await advance(45);assert.equal(inputs(socket).length,count+3);event(key(root,'backspace'),'pointerup');await advance(500);assert.equal(inputs(socket).length,count+3);console.log('PASS repeat immediate / 400ms / 45ms and release');
 const capture=find(root,'live-capture');handle.focus();count=inputs(socket).length;event(capture,'compositionstart');capture.value='ㅎ';event(capture,'input',{isComposing:true});capture.value='한';event(capture,'input',{isComposing:true});assert.equal(inputs(socket).length,count);event(capture,'compositionend',{data:'한'});capture.value='한';event(capture,'input',{isComposing:false});await flush();assert.deepEqual(inputs(socket).slice(count),['한']);
 event(capture,'compositionstart');capture.value='한글';const pending=handle.sendKey('enter');event(capture,'compositionend',{data:'글'});await pending;assert.deepEqual(inputs(socket).slice(-2),['글','\r']);console.log('PASS Korean IME commit once and control-key ordering');
 // iOS 한글 키보드 모델: 조합 이벤트 없이 앞 음절을 지우고(deleteContentBackward) 새 음절을 넣는다. textarea를 비우지 않아야 다음 자모가 앞 글자와 합쳐진다.
 count=inputs(socket).length;handle.sendKey('escape');count++;const type=(value,inputType)=>{const before=event(capture,'beforeinput',{inputType});if(!before.defaultPrevented)capture.value=value;event(capture,'input',{inputType});return before;};
 for(const [value,inputType] of [['ㅎ','insertText'],['','deleteContentBackward'],['하','insertText'],['','deleteContentBackward'],['한','insertText'],['한ㄱ','insertText'],['한','deleteContentBackward'],['한그','insertText'],['한','deleteContentBackward'],['한글','insertText']]){assert.equal(type(value,inputType).defaultPrevented,false);assert.equal(capture.value,value);}
 await flush();assert.deepEqual(inputs(socket).slice(count),['ㅎ','\x7f','하','\x7f','한','ㄱ','\x7f','그','\x7f','글']);
 count=inputs(socket).length;capture.value='한글!';event(capture,'input');capture.value='한글?';event(capture,'input');await flush();assert.deepEqual(inputs(socket).slice(count),['!','\x7f?']);
 count=inputs(socket).length;await handle.sendKey('enter');assert.equal(capture.value,'');assert(type('','deleteContentBackward').defaultPrevented);event(capture,'keydown',{key:'Backspace'});await flush();assert.deepEqual(inputs(socket).slice(count),['\r','\x7f','\x7f']);
 capture.value='a';event(capture,'input');assert.equal(event(capture,'keydown',{key:'Backspace'}).defaultPrevented,false);await flush();count=inputs(socket).length;handle.toggleShift(true);capture.value='ab';event(capture,'input');await flush();assert.deepEqual(inputs(socket).slice(count),['B']);console.log('PASS iOS-style replace-in-place Hangul keeps textarea context, diff sends DEL+syllable, control keys reset context');
 count=inputs(socket).length;event(capture,'compositionstart');handle.setInputMode('buffered');handle.setInputMode('direct');event(capture,'compositionend',{data:'늦음'});capture.value='늦음';event(capture,'input');await flush();assert.equal(inputs(socket).length,count);assert.equal(capture.value,'');
 handle.focus();event(capture,'compositionstart');handle.unmount();event(capture,'compositionend',{data:'이전'});capture.value='이전';event(capture,'input');await flush();assert.equal(inputs(socket).length,count);assert.equal(root.children.length,0);console.log('PASS generation discard on mode switch / unmount');
 ({root,handle,socket}=await mount());handle.setInputMode('buffered');let command=find(root,'command-input');command.value='echo 한글';event(command,'input');event(find(root,'buffered-command'),'submit');await flush();assert.equal(command.value,'echo 한글');let sent=socket.sent.at(-1),r=T.reader(sent.slice(5)),id=r.n();socket.frame(0x27,T.v(id),T.v(0));await flush();assert.equal(command.value,'');
 command.value='saved draft';event(command,'input');handle.unmount();({root,handle,socket}=await mount());assert.equal(handle.getInputMode(),'buffered');assert.equal(find(root,'command-input').value,'saved draft');const mode=find(root,'direct-toggle');assert.equal(mode.textContent,'Buffered input');assert.equal(mode.attrs?.['aria-label']??mode.getAttribute?.('aria-label'),'Buffered input');handle.setInputMode('direct');assert.equal(mode.textContent,'Direct input');console.log('PASS buffered draft retention, opt-out and ACK-only clear');
 let fit=handle.fitToPhone();assert.equal(find(root,'display-mode').getAttribute('aria-pressed'),'false');socket.frame(0x2c,T.v(1));await assert.rejects(fit,/rejected/);assert.equal(find(root,'display-mode').getAttribute('aria-pressed'),'false');fit=handle.fitToPhone();socket.frame(0x2c,T.v(0));await fit;assert.equal(find(root,'display-mode').getAttribute('aria-pressed'),'true');fit=handle.fitToPhone();socket.frame(0x2c,T.v(0));await fit;
 count=socket.sent.filter(a=>a[4]===0x17).length;handle.setFontScale(1.4);assert.equal(socket.sent.filter(a=>a[4]===0x17).length,count);assert.equal(find(root,'terminal-history').style.fontSize,'20px');console.log('PASS Fit ownership ACK, failure rollback and local font sizing');
 handle.loadHistory();sent=socket.sent.at(-1);r=T.reader(sent.slice(5));const req=r.n();assert.equal(r.n(),0);assert.equal(r.n(),0);assert.equal(r.n(),300);const rows=Array.from({length:100},()=>[{ch:'x',zero:'',flags:0}]);const row=C.concat(T.v(1),T.text('x'),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0));socket.frame(0x26,T.v(req),T.v(100),T.v(2),T.v(0),T.v(100),...Array(100).fill(row));handle.loadHistory();r=T.reader(socket.sent.at(-1).slice(5));r.n();assert.equal(r.n(),1);assert.equal(r.n(),1);let historyReq;for(let page=0;page<9;page++){r=T.reader(socket.sent.at(-1).slice(5));historyReq=r.n();socket.frame(0x26,T.v(historyReq),T.v(10000),T.v(2),T.v(page*100),T.v(100),...Array(100).fill(row));handle.loadHistory();}assert.equal(find(root,'terminal-history').textContent.split('\n').length,1001);assert.equal(find(root,'load-history').disabled,true);const before=socket.sent.length;handle.loadHistory();assert.equal(socket.sent.length,before);console.log('PASS 300-row requests, hot-to-cold history and 1000-row cap');
 ({root,handle,socket}=await mount(46));handle.loadHistory();r=T.reader(socket.sent.at(-1).slice(5));const wreq=r.n();const wc=(ch,zero,flags)=>C.concat(T.text(ch),T.v(zero.length),...zero.map(z=>T.text(z)),T.v(0),T.v(0),T.v(0),T.v(flags));
 socket.frame(0x26,T.v(wreq),T.v(1),T.v(1),T.v(0),T.v(1),C.concat(T.v(6),wc('a',[],0),wc('ᄒ',['ᅡ','ᆫ'],8),wc(' ',[],16),wc('글',[],8),wc(' ',[],16),wc('b',[],0)));
 const hist=find(root,'terminal-history'),span=hist.children[0].children.find(c=>typeof c==='object');assert.equal(hist.textContent,'a한글b\n');assert.equal(span.textContent,'한글');assert.equal(span.style.letterSpacing,'var(--wide-spacing)');assert.match(handle.text(),/^a한글b/);handle.unmount();console.log('PASS history <pre>: NFD joined to NFC, wide runs spaced to two cells');
 handle.unmount();let lastStatus;({root,handle,socket}=await mount(99,{onStatus:s=>{lastStatus=s;}}));const uncertain=handle.sendKey('ctrlC');socket.close();await assert.rejects(uncertain,/Input delivery is uncertain; input will not be resent\./);assert.match(lastStatus.message,/will not be resent/);assert.equal(inputs(socket).length,1);handle.unmount();console.log('PASS disconnect uncertainty, no retry and listener cleanup');
 ({root,handle,socket}=await mount());const canvas=find(root,'terminal-grid'),calls=canvas.calls;await advance(16);
 const line=(n,text)=>C.concat(T.v(2),T.v(n),T.v([...text].length),...[...text].map(ch=>C.concat(T.text(ch),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0)))),cursor=(line,column)=>C.concat(T.v(3),T.v(line),T.v(column),T.v(1));
 const update=(...ops)=>socket.frame(0x21,T.v(2),T.v(2),T.v(1),T.v(ops.length),...ops),drawn=async()=>{const before={...calls};await advance(16);return {text:calls.fillText-before.fillText,font:calls.font-before.font,draws:calls.draws-before.draws};};
 update(C.concat(T.v(0),T.v(80),T.v(24)),line(0,'abc'),line(1,'def'),line(2,'ghi'),cursor(1,2));assert.deepEqual(await drawn(),{text:9,font:0,draws:1});
 update(line(1,'xy'));assert.deepEqual(await drawn(),{text:2,font:0,draws:1});
 update(cursor(2,0));assert.equal((await drawn()).text,5);
 update(line(0,'a'));update(line(2,'g'));assert.deepEqual(await drawn(),{text:2,font:0,draws:1});
 update(C.concat(T.v(0),T.v(80),T.v(24)),line(0,'abc'),line(3,'jk'));assert.equal((await drawn()).text,5);
 handle.setFontScale(1.25);assert.deepEqual(await drawn(),{text:5,font:1,draws:1});assert.equal(canvas.width,Math.round(80*8*16/13));
 const wide=(ch,zero)=>C.concat(T.text(ch),T.v(zero.length),...zero.map(z=>T.text(z)),T.v(0),T.v(0),T.v(0),T.v(8)),spacer=C.concat(T.text(' '),T.v(0),T.v(0),T.v(0),T.v(0),T.v(16));
 update(C.concat(T.v(2),T.v(5),T.v(8),wide('ᄒ',['ᅡ','ᆫ']),spacer,wide('ᄀ',['ᅳ','ᆯ']),spacer,wide('한',[]),spacer,wide('글',[]),spacer));calls.texts=[];await drawn();assert.deepEqual(calls.texts,['한','글','한','글']);calls.texts=null;await advance(500);assert.match(canvas.getAttribute('aria-label'),/\n한글한글\n/);assert.match(handle.text(),/^한글한글$/m);
 await advance(500);assert.match(canvas.getAttribute('aria-label'),/^abc\n\n\njk/);handle.unmount();
 console.log('PASS dirty-row canvas redraw: one changed row only, cursor rows, one frame per batch, Reset/font full');
 ({root,handle,socket}=await mount(42));const port=find(root,'terminal-scroll'),capture2=find(root,'live-capture'),command2=find(root,'command-input');let focused=0,commandFocused=0;capture2.addEventListener('focus',()=>focused++);command2.addEventListener('focus',()=>commandFocused++);
 assert.equal(root.dataset.phase,'ready');
 // 터미널 탭·스크롤은 키보드를 열지 않는다. 키 줄의 키보드 버튼만 연다.
 event(port,'pointerdown',{clientX:100,clientY:100});event(port,'pointermove',{clientX:104,clientY:103});event(port,'click');assert.equal(focused,0);
 event(port,'pointerdown',{clientX:100,clientY:100});event(port,'pointermove',{clientX:100,clientY:130});event(port,'click');assert.equal(focused,0);
 event(find(root,'show-keyboard'),'click');assert.equal(focused,1);
 port.scrollLeft=50;const touch=(type,x,y)=>event(port,type,{touches:x==null?[]:[{clientX:x,clientY:y}]});
 touch('touchstart',200,400);let te=touch('touchmove',190,330);assert.equal(te.defaultPrevented,false);touch('touchmove',150,200);assert.equal(port.scrollLeft,50);touch('touchend');
 touch('touchstart',200,400);te=touch('touchmove',190,398);assert(te.defaultPrevented);te=touch('touchmove',120,390);assert(te.defaultPrevented);assert.equal(port.scrollLeft,130);touch('touchend');
 socket.frame(0x2b,Uint8Array.of(0),T.v(18));socket.autoAck=true;touch('touchstart',200,100);touch('touchmove',200,110);te=touch('touchmove',201,170);assert(te.defaultPrevented);touch('touchend');await flush();assert.match(inputs(socket).join(''),/^(\x1b\[<64;\d+;\d+M)+$/);socket.autoAck=false;socket.sent.length=0;socket.frame(0x2b,Uint8Array.of(0),T.v(0));
 te=event(port,'wheel',{deltaX:30,deltaY:2});assert(te.defaultPrevented);assert.equal(port.scrollLeft,160);event(port,'wheel',{deltaX:1,deltaY:40});assert.equal(port.scrollLeft,160);
 socket.autoAck=true;for(let i=0;i<5;i++){socket.screen(`flow ${i} `+'w'.repeat(70));await advance(16);}await handle.sendKey('enter');assert.equal(port.scrollLeft,160);
 assert.match(handle.text(),/flow 4 w+$/);
 context.document.activeElement=capture2;handle.setInputMode('buffered');assert.equal(commandFocused,1);context.document.activeElement=null;handle.setInputMode('direct');assert.equal(focused,1);
 socket.close();assert.equal(root.dataset.phase,'disconnected');handle.unmount();assert.equal(root.dataset.phase,undefined);
 ({root,handle,socket}=await mount(43));let statuses=[];handle.unmount();({root,handle,socket}=await mount(44,{onStatus:st=>statuses.push(st)}));context.document.activeElement=find(root,'live-capture');socket.close();assert.equal(statuses.at(-1).typing,true);context.document.activeElement=null;handle.unmount();
 ({root,handle,socket}=await mount(45));const port45=find(root,'terminal-scroll');port45.scrollTop=10;event(port45,'scroll');assert.equal(socket.sent.filter(a=>a[4]===0x12).length,0);port45.scrollLeft=90;port45.clientHeight=360;let fitting=handle.fitToPhone();let fitRole=T.reader(socket.sent.filter(a=>a[4]===0x17).at(-1).slice(5));assert.equal(fitRole.n(),1);assert.equal(fitRole.n(),48);assert.equal(fitRole.n(),20);socket.frame(0x2c,T.v(0));await fitting;assert.equal(root.dataset.fit,'true');assert.equal(port45.scrollLeft,0);const roles=()=>socket.sent.filter(a=>a[4]===0x17).length;let before17=roles();socket.frame(0x21,T.v(3),T.v(3),T.v(1),T.v(1),C.concat(T.v(0),T.v(48),T.v(20)));await flush();viewport.offsetTop=5;event(viewport,'scroll');viewport.offsetTop=0;await flush();assert.equal(roles(),before17);handle.unmount();
 console.log('PASS disconnect reports typing for refocus, Fit fits cols+rows, resets scrollLeft, and skips same-size resize roles');
 console.log('PASS terminal tap keeps keyboard closed, one-axis touch pan (no native horizontal), output/input keep scrollLeft, mode-switch refocus, text() and phase');
 // 폰 기본 Fit: 저장된 선택이 켜짐이면 연결 직후 자동으로 맞추고(저장은 하지 않음), 사용자가 끄면 그 선택을 저장한다.
 let prefs=[];({root,handle,socket}=await mount(47,{preferences:{fitToPhone:true},onPreferenceChange:p=>prefs.push(p)}));let port47=find(root,'terminal-scroll');const roles47=()=>socket.sent.filter(a=>a[4]===0x17).map(a=>[...a.slice(5)]);
 assert.deepEqual(roles47().at(-1),[1,48,24]);port47.clientHeight=540;socket.frame(0x2c,T.v(0));await flush();assert.equal(root.dataset.fit,'true');assert.deepEqual(prefs,[]);
 // Fit 응답을 기다리는 동안 바뀐 높이(상태줄이 사라짐 등)도 응답 뒤에 맞춘다.
 await advance(150);assert.deepEqual(roles47().at(-1),[1,48,30]);socket.frame(0x2c,T.v(0));await flush();
 // 키보드가 열려 높이가 줄어도 행은 그대로, 이벤트가 몰려도 멈춘 뒤 한 번만 맞춘다.
 port47.clientHeight=360;event(viewport,'resize');event(viewport,'resize');await advance(16);await advance(149);let n47=roles47().length;await advance(1);assert.equal(roles47().length,n47+1);assert.deepEqual(roles47().at(-1),[1,48,20]);socket.frame(0x2c,T.v(0));socket.frame(0x21,T.v(4),T.v(4),T.v(1),T.v(1),C.concat(T.v(0),T.v(48),T.v(20)));await flush();
 viewport.height=544;port47.clientHeight=60;event(viewport,'resize');await advance(200);assert.equal(roles47().length,n47+1);assert.equal(root.dataset.keyboardOpen,'true');
 viewport.height=844;port47.clientHeight=360;event(viewport,'resize');await advance(200);assert.equal(roles47().length,n47+1);
 port47.clientHeight=540;event(viewport,'resize');await advance(200);assert.deepEqual(roles47().at(-1),[1,48,30]);socket.frame(0x2c,T.v(0));await flush();
 // Fold/rotation events refit width too. If the next fold arrives before the
 // role acknowledgement, retain it and fit the final viewport after the reply.
 port47.clientWidth=830;event(globalEvents,'orientationchange');await advance(16+150);assert.deepEqual(roles47().at(-1),[1,103,30]);
 port47.clientWidth=320;event(globalEvents,'orientationchange');await advance(150);socket.frame(0x2c,T.v(0));await flush();await advance(150);assert.deepEqual(roles47().at(-1),[1,39,30]);socket.frame(0x2c,T.v(0));await flush();
 port47.clientWidth=390;event(viewport,'resize');await advance(16+150);assert.deepEqual(roles47().at(-1),[1,48,30]);socket.frame(0x2c,T.v(0));await flush();
 console.log('PASS fold/rotation refits width and retains the latest viewport during a pending resize');
 port47.clientWidth=0;n47=roles47().length;event(viewport,'resize');await advance(200);assert.equal(roles47().length,n47);port47.clientWidth=390;
 // 화면이 가려지면 크기 소유를 놓고(데스크톱 크기 복귀), 다시 보이면 맞춘다.
 context.document.visibilityState='hidden';event(context.document,'visibilitychange');assert.deepEqual(roles47().at(-1),[0,2,0]);socket.frame(0x2c,T.v(0));context.document.visibilityState='visible';event(context.document,'visibilitychange');assert.deepEqual(roles47().at(-1),[1,48,30]);socket.frame(0x2c,T.v(0));await flush();
 let off=handle.fitToPhone();socket.frame(0x2c,T.v(0));await off;assert.equal(root.dataset.fit,'false');assert.equal(JSON.stringify(prefs),"[{\"fitToPhone\":false}]");handle.unmount();
 ({root,handle,socket}=await mount(48,{preferences:{fitToPhone:false}}));assert.equal(JSON.stringify(socket.sent.filter(a=>a[4]===0x17).map(a=>[...a.slice(5)])),"[[0,2,0]]");handle.unmount();
 console.log('PASS phone Fit by default: auto-fit on connect, debounced refit, keyboard keeps rows, hidden releases size, opt-out saved');
 // 바닥에 있으면 화면 크기가 바뀌어도 바닥을 따라가고, 위로 올려 보는 중이면 위치를 유지한다.
 ({root,handle,socket}=await mount(49));port47=find(root,'terminal-scroll');port47.scrollHeight=1000;port47.clientHeight=640;await advance(16);assert.equal(port47.scrollTop,1000);
 port47.scrollTop=100;event(port47,'scroll');socket.frame(0x21,T.v(5),T.v(5),T.v(1),T.v(1),C.concat(T.v(0),T.v(80),T.v(30)));port47.scrollHeight=1108;await advance(16);assert.equal(port47.scrollTop,100);
 for(let i=0;i<5;i++){socket.screen(`flow ${i}`);await advance(16);}assert.equal(port47.scrollTop,100);
 port47.scrollTop=468;event(port47,"scroll");socket.frame(0x21,T.v(6),T.v(6),T.v(1),T.v(1),C.concat(T.v(0),T.v(80),T.v(40)));port47.scrollHeight=1288;await advance(16);assert.equal(port47.scrollTop,1288);handle.unmount();
 console.log('PASS stick-to-bottom only while at bottom; scrolled-up position kept through output and resize');
 const warmSocket=new context.FakeSocket(),warmRoot=new Element('div');
 const warmHandle=api.mount(warmRoot,{sessionId:42,connect:()=>({socket:warmSocket,credentials:{device_id:'fixture',credential:''},messages:[JSON.stringify({version:2,challenge:''})]})});
 await flush();assert.equal(warmSocket.sent.length,5);assert.equal(warmSocket.sent[4][4],0x10);warmHandle.unmount();
 const authWarmSocket=new context.FakeSocket(),authWarmRoot=new Element('div');
 const authWarmHandle=api.mount(authWarmRoot,{sessionId:43,connect:()=>({socket:authWarmSocket,credentials:{device_id:'fixture',credential:''},channel:new C.Channel(),messages:[C.enc.encode('{}'),T.frame(1,C.concat(T.v(1),T.v(1),T.v((1n<<56n)|4096n|5n)))]})});
 await flush();assert.equal(authWarmSocket.sent.length,2);assert.equal(authWarmSocket.sent[0][4],0x17);assert.equal(authWarmSocket.sent[1][4],0x10);authWarmSocket.frame(0x2c,T.v(0));authWarmSocket.screen();await flush();assert.equal(find(authWarmRoot,'terminal-status').textContent,'80 × 24');authWarmHandle.unmount();
 console.log('PASS prewarmed terminal WebSocket replays buffered challenge or pre-authenticated channel immediately on mount');
 {// 자동 Fit: 크기 요청이 첫 화면을 기다리지 않고 attach와 함께 나가고, 첫 화면 뒤 fitToPhone은 같은 응답을 기다린다(중복 요청 없음).
  const root=new Element('div'),handle=context.mountFixture(root,11,{preferences:{fitToPhone:true}});await flush();const socket=context.sockets.at(-1);find(root,'terminal-scroll').clientHeight=600;socket.handshake();await flush();
  const resizes=()=>socket.sent.filter(a=>a[4]===0x17&&a[5]===1).map(a=>[...a.slice(5)]);assert.equal(JSON.stringify(resizes()),"[[1,48,33]]");
  socket.screen();await flush();assert.equal(find(root,'display-mode').getAttribute('aria-pressed'),'false');socket.frame(0x2c,T.v(0));await flush();
  assert.equal(find(root,'display-mode').getAttribute('aria-pressed'),'true');assert.equal(resizes().length,1);handle.unmount();console.log('PASS auto-fit resize pipelined with attach, no duplicate after first frame');}
 await testHistoryGestures();
 await testHistoryPrefetch();
 await testScrollAnchor();
 await testEmptyHistoryHidden();
 await testFailurePaths();
 console.log('All terminal adapter regression checks passed.');
}
async function testHistoryPrefetch(){
 const {root,handle,socket}=await mount(1300),port=find(root,'terminal-scroll');port.clientHeight=400;port.scrollHeight=20000;port.scrollTop=19600;event(port,'scroll');
 const requests=()=>socket.sent.filter(a=>a[4]===0x12),row=C.concat(T.v(1),T.text('x'),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0));
 const reply=skip=>{const r=T.reader(requests().at(-1).slice(5));socket.frame(0x26,T.v(r.n()),T.v(5000),T.v(1),T.v(skip),T.v(300),...Array(300).fill(row));};
 handle.loadHistory();reply(0);const first=find(root,'terminal-history').children[0];await advance(0);assert.equal(requests().length,2);reply(300);assert.equal(find(root,'terminal-history').children[300],first);await advance(0);assert.equal(requests().length,3);reply(600);await advance(0);assert.equal(requests().length,3);assert.equal(find(root,'terminal-history').children.length,900);
 port.scrollTop=1000;event(port,'scroll');assert.equal(requests().length,4);assert(port.scrollTop>0);reply(900);assert.equal(find(root,'terminal-history').children.length,1000);assert.equal(find(root,'terminal-history').children[900],first);handle.unmount();await advance(0);assert.equal(requests().length,4);
 const hidden=await mount(1301,{visible:false});find(hidden.root,'terminal-scroll').clientHeight=400;hidden.handle.loadHistory();let r=T.reader(hidden.socket.sent.at(-1).slice(5));hidden.socket.frame(0x26,T.v(r.n()),T.v(5000),T.v(1),T.v(0),T.v(300),...Array(300).fill(row));await advance(0);assert.equal(hidden.socket.sent.filter(a=>a[4]===0x12).length,1);hidden.handle.unmount();
 const partial=await mount(1302),pp=find(partial.root,'terminal-scroll');pp.clientHeight=400;pp.scrollHeight=20000;partial.handle.loadHistory();
 const pr=()=>partial.socket.sent.filter(a=>a[4]===0x12).at(-1),respond=(total,skip,n)=>{const r=T.reader(pr().slice(5));partial.socket.frame(0x26,T.v(r.n()),T.v(total),T.v(1),T.v(skip),T.v(n),...Array(n).fill(row));};respond(160,0,160);await advance(0);respond(5000,0,300);await advance(0);respond(5000,300,300);await advance(0);r=T.reader(pr().slice(5));r.n();r.n();r.n();r.n();assert.equal(r.n(),140);respond(5000,600,140);await advance(0);assert.equal(find(partial.root,'terminal-history').children.length,900);assert.equal(find(partial.root,'terminal-grid').hidden,false);partial.handle.unmount();
 console.log('PASS 900 rows warm before scrolling, 3-viewport lookahead, existing DOM retained, 1000-row bound, hidden/unmounted terminals stop warming');
}
// 새 세션처럼 빈 기록을 자동으로 처음 불러올 때는 기록 줄이 잠깐 보였다 사라지며 터미널을 밀지 않는다. 행이 온 뒤에는 보인다.
async function testEmptyHistoryHidden(){
 const {root,handle,socket}=await mount(1500),controls=find(root,'terminal-history-controls'),requests=()=>socket.sent.filter(a=>a[4]===0x12),row=C.concat(T.v(1),T.text('x'),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0)),reply=(total,pages,n)=>{const r=T.reader(requests().at(-1).slice(5));socket.frame(0x26,T.v(r.n()),T.v(total),T.v(pages),T.v(0),T.v(n),...Array(n).fill(row));};
 handle.loadHistory();assert.equal(requests().length,1);assert.equal(controls.hidden,true);reply(0,0,0);assert.equal(controls.hidden,true);
 socket.screen('out 0');await flush();handle.loadHistory();assert.equal(controls.hidden,true);reply(5,1,5);assert.equal(controls.hidden,false);socket.screen('out 1');await flush();handle.loadHistory(true);assert.equal(controls.hidden,false);handle.unmount();
 console.log('PASS empty automatic history load never shows the history row; loaded rows do');
}
// 레이아웃을 흉내 낸 스크롤 포트(scrollTop 고정 범위, 히스토리 행 18px + 캔버스 400px)로 손가락 스크롤 위치 보존을 확인한다.
async function testScrollAnchor(){
 const {root,handle,socket}=await mount(1400),port=find(root,'terminal-scroll'),hist=find(root,'terminal-history'),grid=find(root,'terminal-grid'),controls=find(root,'terminal-history-controls'),latest=find(root,'terminal-latest');let top=0;port.clientHeight=400;
 Object.defineProperty(grid,'offsetTop',{get:()=>(controls.hidden?0:32)+hist.children.length*18});Object.defineProperty(port,'scrollHeight',{get:()=>grid.offsetTop+(grid.hidden?0:400)});Object.defineProperty(port,'scrollTop',{get:()=>top,set:v=>{top=Math.max(0,Math.min(v,port.scrollHeight-port.clientHeight));}});
 const requests=()=>socket.sent.filter(a=>a[4]===0x12),row=C.concat(T.v(1),T.text('x'),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0)),reply=(total,pages,n)=>{const r=T.reader(requests().at(-1).slice(5));socket.frame(0x26,T.v(r.n()),T.v(total),T.v(pages),T.v(0),T.v(n),...Array(n).fill(row));};
 const gap=()=>port.scrollHeight-top-port.clientHeight,drag=dy=>{port.scrollTop-=dy;event(port,'scroll');};
 handle.loadHistory();reply(100,0,100);assert.equal(gap(),0);
 // 바닥에서 위로 10px 끌면 새로고침 요청이 나가고, 응답이 와도(그 사이 출력 20행이 스크롤백으로 넘어감) 바닥으로 끌려가지 않는다.
 socket.screen('out 0');await flush();event(port,'touchstart',{touches:[{clientX:150,clientY:200}]});event(port,'touchmove',{touches:[{clientX:150,clientY:210}]});drag(10);
 const n=requests().length;reply(120,2,120);assert.equal(gap(),10);assert.equal(latest.hidden,false);event(port,'touchend',{touches:[]});
 for(let i=1;i<6;i++){socket.screen(`out ${i}`);await advance(16);}assert.equal(gap(),10);assert.equal(latest.hidden,false);assert.equal(requests().length,n);
 // 맨 위 근처까지 올리면 cold 페이지를 앞에 붙이고, 보던 줄(캔버스 기준 위치)은 그대로다.
 drag(top-100);assert.equal(requests().length,n+1);const rel=grid.offsetTop-top;reply(300,2,300);assert.equal(grid.offsetTop-top,rel);socket.screen('out 6');await advance(16);assert.equal(grid.offsetTop-top,rel);
 // 손으로 바닥까지 내리면 다시 따라가고 Latest 버튼이 숨는다.
 port.scrollTop=1e9;event(port,'scroll');assert.equal(latest.hidden,true);handle.unmount();
 console.log('PASS upward drag from live keeps position through hot refresh and new output; cold prepend keeps anchor; reaching bottom re-sticks');
}
async function testHistoryGestures(){
 const {root,handle,socket}=await mount(1201),port=find(root,'terminal-scroll');port.clientHeight=400;port.scrollHeight=400;
 const requests=()=>socket.sent.filter(a=>a[4]===0x12),last=()=>{const r=T.reader(requests().at(-1).slice(5));return {id:r.n(),source:r.n(),skip:r.n(),count:r.n()};};
 const cell=ch=>C.concat(T.text(ch),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0)),row=ch=>C.concat(T.v(1),cell(ch));
 const reply=(total,pages,skip,rows)=>socket.frame(0x26,T.v(last().id),T.v(total),T.v(pages),T.v(skip),T.v(rows.length),...rows.map(row));
 handle.loadHistory();reply(0,0,0,[]);const n=requests().length;handle.loadHistory();assert.equal(requests().length,n);
 // Later output revives history even after the first hot/cold snapshot was empty.
 socket.screen('new output');await flush();event(port,'touchstart',{touches:[{clientX:150,clientY:200}]});event(port,'touchmove',{touches:[{clientX:150,clientY:220}]});assert.equal(requests().length,n+1);assert.equal(last().source,0);reply(1,0,0,['a']);assert.equal(find(root,'terminal-history').textContent,'a\n');assert.equal(find(root,'terminal-latest').hidden,false);event(port,'touchend',{touches:[]});
 // Already-loaded history still accepts pulling at the top, without requiring
 // a native scroll event (a fitted canvas can fill the viewport exactly).
 socket.screen('newer output');port.scrollHeight=800;port.scrollTop=400;event(port,'scroll');event(port,'touchstart',{touches:[{clientX:150,clientY:200}]});event(port,'touchmove',{touches:[{clientX:150,clientY:230}]});port.scrollTop=2000;reply(2,1,0,['b','c']);assert.equal(find(root,'terminal-history').textContent,'b\nc\n');event(port,'touchend',{touches:[]});
 handle.loadHistory(false,true);reply(0,1,0,[]);assert.equal(find(root,'terminal-history').textContent,'b\nc\n');assert.equal(last().source,1);reply(5000,1,0,Array(300).fill('d'));
 port.scrollTop=0;event(port,'touchstart',{touches:[{clientX:150,clientY:200}]});event(port,'touchmove',{touches:[{clientX:150,clientY:220}]});assert.equal(last().source,1);reply(5000,1,300,Array(300).fill('e'));event(port,'touchend',{touches:[]});
 for(let i=2;i<5;i++){handle.loadHistory();reply(5000,1,i*300,Array(300).fill(String(i)));}
 assert.equal(find(root,'terminal-history').textContent.split('\n').length,1001);assert.equal(find(root,'terminal-grid').hidden,true);assert.equal(find(root,'load-history').disabled,false);
 const before=requests().length;handle.loadHistory();assert.equal(requests().length,before+1);reply(5000,1,1500,Array(300).fill('z'));assert.equal(find(root,'terminal-history').textContent[0],'z');assert.equal(find(root,'terminal-history').textContent.split('\n').length,1001);
 event(find(root,'terminal-latest'),'click');assert.equal(find(root,'terminal-grid').hidden,false);assert.equal(last().source,0);reply(2,1,0,['b','c']);assert.equal(find(root,'terminal-latest').hidden,true);assert.equal(port.scrollTop,port.scrollHeight);handle.unmount();
 console.log('PASS empty history refreshes after output, upward gestures reload hot and cold, bounded 1000-row window keeps paging, Latest restores live canvas');
 context.innerWidth=390;const phone=await mount(1202,{preferences:{fitToPhone:false}});assert.equal(phone.handle.getInputMode(),'direct');let focused=0;find(phone.root,'live-capture').addEventListener('focus',()=>focused++);event(find(phone.root,'terminal-scroll'),'click');assert.equal(focused,0);phone.handle.setInputMode('buffered');phone.handle.unmount();const saved=await mount(1202,{preferences:{fitToPhone:false}});assert.equal(saved.handle.getInputMode(),'buffered');saved.handle.unmount();delete context.innerWidth;
 console.log('PASS phone defaults to live direct input, terminal taps keep the keyboard closed, saved buffered preference survives');
 const retry=await mount(1203),rp=find(retry.root,'terminal-scroll'),rs=retry.socket;rp.clientHeight=400;rp.scrollHeight=1000;rp.scrollTop=600;event(rp,'scroll');retry.handle.loadHistory();
 const historyRequests=()=>rs.sent.filter(a=>a[4]===0x12),answer=()=>{const r=T.reader(historyRequests().at(-1).slice(5));rs.frame(0x26,T.v(r.n()),T.v(1),T.v(1),T.v(0),T.v(1),row('a'));};answer();
 rs.screen('resize');event(rp,'wheel',{deltaX:0,deltaY:-20});rp.scrollTop=0;event(rp,'scroll');const count=historyRequests().length;answer();assert.equal(historyRequests().length,count+1);const next=T.reader(historyRequests().at(-1).slice(5));next.n();assert.equal(next.n(),1);retry.handle.unmount();
 console.log('PASS stale hot refresh at scrollTop zero continues into cold history without another native scroll event');

}
async function testFailurePaths(){
 let {root,handle,socket}=await mount(701,{preferences:{inputMode:'direct'},onPhoto:()=>{}});socket.autoAck=true;handle.focus();let capture=find(root,'live-capture');
 const hello=socket.sent.find(a=>a[4]===2),hr=T.reader(hello.slice(5));hr.n();hr.n();assert(hr.u()&8192n);
 const photo=find(root,'photo-attach'),accessory=find(root,'accessory-row');assert.equal(photo.parent,accessory,'Direct mode exposes the photo action outside its hidden buffered form');assert.equal(accessory.hidden,undefined);assert.equal(find(root,'buffered-command').hidden,true);let picked=0;find(root,'photo-picker').click=()=>picked++;event(photo,'click');assert.equal(picked,1,'Direct photo action opens its existing image picker without sending input');assert.equal(inputs(socket).length,0);assert.equal(find(root,'photo-picker').accept,'image/*');assert.equal(find(root,'file-picker').accept,'image/*');
 socket.frame(0x2b,Uint8Array.of(0),T.v(32));event(capture,'paste',{clipboardData:{getData:()=>'one\r\ntwo\x1b[201~'}});await flush();assert.deepEqual(inputs(socket),['\x1b[200~one\ntwo[201~\x1b[201~']);
 capture.value='fallback\npaste';event(capture,'input',{inputType:'insertFromPaste'});await flush();assert.equal(inputs(socket).at(-1),'\x1b[200~fallback\npaste\x1b[201~');
 socket.frame(0x2b,Uint8Array.of(0),T.v(0));capture.value='review\nsecond';event(capture,'input',{inputType:'insertFromPaste'});await flush();const review=root.children.at(-1).children.at(-1),editor=review.children[0].children[0];assert.equal(editor.tagName,'textarea');assert.equal(editor.value,'review\nsecond');const before=inputs(socket).length;assert.equal(before,2);
 event(editor,'compositionstart');event(review,'submit');assert.equal(inputs(socket).length,before);event(editor,'compositionend');editor.value='edited\nsecond';event(review,'submit');await flush();assert.equal(inputs(socket).at(-1),'edited\rsecond');
 handle.focus();capture.value='cancel\nthis';event(capture,'input',{inputType:'insertFromPaste'});const cancelled=root.children.at(-1).children.at(-1);event(cancelled.children[2],'click');event(cancelled,'submit');await flush();assert.equal(inputs(socket).length,before+1);
 // Missing initial modes is safe even if the server advertises the feature.
 handle.unmount();({root,handle,socket}=await mount(702,{preferences:{inputMode:'direct'}}));handle.focus();capture=find(root,'live-capture');capture.value='unknown\nmode';event(capture,'input',{inputType:'insertFromPaste'});assert.equal(inputs(socket).length,0);assert.equal(root.children.at(-1).children.at(-1).tagName,'form');handle.setInputMode('buffered');assert.equal(root.children.at(-1).children.length,3);handle.unmount();
 // A server that did not negotiate modes cannot enable automatic paste framing.
 root=new Element('div');handle=context.mountFixture(root,703,{preferences:{inputMode:'direct'}});await flush();socket=context.sockets.at(-1);socket.handshake(4096n|5n);socket.screen();socket.frame(0x2b,Uint8Array.of(0),T.v(32));handle.focus();capture=find(root,'live-capture');capture.value='legacy\nmode';event(capture,'input',{inputType:'insertFromPaste'});assert.equal(inputs(socket).length,0);handle.unmount();
 console.log('PASS TR-A03/16 negotiated bracketed paste, CRLF/control sanitization, multiline review/IME/cancel, unsupported/unknown modes and image-only pickers');
 ({root,handle,socket}=await mount(704,{preferences:{inputMode:'direct'}}));handle.focus();capture=find(root,'live-capture');capture.value='a';event(capture,'input');socket.frame(0x27,T.v(1),T.v(2));await flush();assert.equal(capture.value,'');assert.match(find(root,'terminal-status').textContent,/rejected/);socket.screen();assert.match(find(root,'terminal-status').textContent,/rejected/);capture.value='ab';event(capture,'input');await flush();assert.deepEqual(inputs(socket),['a']);await assert.rejects(handle.sendKey('enter'),/Resume/);
 const recover=root.children[0].children.find(e=>e.textContent==='Resume input');assert.equal(recover.hidden,false);event(recover,'click');socket.autoAck=true;capture.value='ab';event(capture,'input');await flush();assert.deepEqual(inputs(socket),['a','ab']);socket.autoAck=false;const rejected=handle.sendKey('enter');socket.frame(0x27,T.v(3),T.v(2));await assert.rejects(rejected,/rejected/);socket.close();assert.equal(recover.hidden,true);event(recover,'click');assert.equal(root.dataset.phase,'disconnected');handle.unmount();
 console.log('PASS TR-A04 rejected capture invalidates context, blocks stale edits/control keys and explicitly resumes without retransmission');
 let status;({root,handle,socket}=await mount(705,{preferences:{inputMode:'buffered'},onStatus:s=>status=s}));const command=find(root,'command-input');command.value='uncertain';event(command,'input');event(find(root,'buffered-command'),'submit');await flush();assert.equal(find(root,'command-send').disabled,true);await advance(1000);assert.match(status.message,/Waiting for input/);assert.equal(root.dataset.phase,'input-pending');socket.screen();assert.equal(root.dataset.phase,'input-pending');assert.match(find(root,'terminal-status').textContent,/Waiting for input/);await advance(9000);assert.equal(status.phase,'disconnected');assert.match(status.message,/will not be resent/);assert.equal(find(root,'command-send').disabled,false);assert.equal(command.value,'uncertain');assert.equal(inputs(socket).length,1);socket.frame(0x27,T.v(1),T.v(0));await flush();assert.equal(command.value,'uncertain');handle.unmount();
 console.log('PASS TR-A05 stalled application ACK deadline, buffered draft retained, no duplicate input and late ACK ignored');
 ({root,handle,socket}=await mount(705,{preferences:{inputMode:'buffered'}}));assert.equal(context.KrillMobile.terminal.updateBusy('705'),true);assert.equal(find(root,'command-input').value,'uncertain');assert.match(find(root,'terminal-status').textContent,/uncertain/);await assert.rejects(handle.sendKey('enter'),/Resume/);assert.deepEqual(inputs(socket),[]);event(root.children[0].children.find(e=>e.textContent==='Resume input'),'click');assert.equal(context.KrillMobile.terminal.updateBusy('705'),false);assert.deepEqual(inputs(socket),[]);handle.unmount();
 console.log('PASS uncertain terminal delivery survives automatic remount and blocks updates until explicit Resume input');
 ({root,handle,socket}=await mount(706,{preferences:{inputMode:'direct',fitToPhone:true}}));socket.frame(0x2c,T.v(0));await flush();const hide=handle.setVisible(false);await flush();assert.deepEqual([...socket.sent.at(-1).slice(5)],[0,2,0]);socket.frame(0x2c,T.v(0));await hide;const count=socket.sent.length;event(viewport,'resize');await advance(200);assert.equal(socket.sent.length,count);await assert.rejects(handle.sendKey('enter'),/Resume/);assert.equal(socket.readyState,1);const show=handle.setVisible(true);await flush();assert.equal(socket.sent.at(-1)[5],1);socket.frame(0x2c,T.v(0));await show;handle.unmount();
 ({root,handle,socket}=await mount(707,{visible:false,preferences:{fitToPhone:true}}));assert(!socket.sent.some(a=>a[4]===0x17&&a[5]===1));handle.unmount();
 ({root,handle,socket}=await mount(709,{preferences:{fitToPhone:true}}));const pendingHide=handle.setVisible(false);socket.frame(0x2c,T.v(0));await flush();assert.deepEqual([...socket.sent.at(-1).slice(5)],[0,2,0]);socket.frame(0x2c,T.v(0));await pendingHide;const rolesBefore=socket.sent.filter(a=>a[4]===0x17).length;await advance(200);assert.equal(socket.sent.filter(a=>a[4]===0x17).length,rolesBefore);handle.unmount();
 console.log('PASS TR-A10 hidden Chat releases size, suppresses refits/input, preserves socket and restores Fit; initially hidden never claims size');
 ({root,handle,socket}=await mount(708,{preferences:{inputMode:'direct'}}));socket.autoAck=true;handle.focus();capture=find(root,'live-capture');for(const [physical,id] of [['Enter','enter'],['Tab','tab'],['ArrowUp','arrowUp'],['ArrowDown','arrowDown'],['ArrowLeft','arrowLeft'],['ArrowRight','arrowRight']]){event(capture,'keydown',{key:physical,shiftKey:true});await flush();const encoded=inputs(socket).at(-1);handle.toggleShift(true);await handle.sendKey(id);assert.equal(inputs(socket).at(-1),encoded);}handle.unmount();
 console.log('PASS TR-A15 physical Shift+Enter/Tab/arrows and accessory key encoder match');
 {let edited=0;({root,handle,socket}=await mount(710,{preferences:{inputMode:'buffered',orderedKeyIds:['ctrlC','enter'],visibleKeyIds:['enter','ctrlC']},onPhoto:()=>{},onEditKeys:()=>edited++}));
 const editor=find(root,'command-input');assert.equal(editor.tagName,'textarea');assert.equal(editor.rows,1);assert.equal(editor.getAttribute('enterkeyhint'),'enter');
 assert.equal(event(editor,'keydown',{key:'Enter'}).defaultPrevented,false);editor.value='첫 줄\n둘째 줄';event(editor,'input');assert.equal(inputs(socket).length,0);
 event(editor,'compositionstart');event(find(root,'buffered-command'),'submit');assert.equal(inputs(socket).length,0);event(editor,'compositionend');
 socket.frame(0x2b,Uint8Array.of(0),T.v(32));event(find(root,'buffered-command'),'submit');event(find(root,'buffered-command'),'submit');await flush();assert.deepEqual(inputs(socket),['\x1b[200~첫 줄\n둘째 줄\x1b[201~\r']);assert.equal(editor.value,'첫 줄\n둘째 줄');
 editor.value+='\n새 초안';event(editor,'input');socket.frame(0x27,T.v(1),T.v(0));await flush();assert.equal(editor.value,'첫 줄\n둘째 줄\n새 초안');
 const keys=find(root,'keybar');assert.deepEqual(keys.children.filter(b=>b.dataset.keyId&&!['shift','ctrl','alt'].includes(b.dataset.keyId)).map(b=>b.dataset.keyId),['ctrlC','enter']);assert.equal(keys.children.at(-1),find(root,'edit-keys'));event(find(root,'edit-keys'),'click');assert.equal(edited,1);handle.unmount();
 console.log('PASS buffered multiline: Enter never sends, IME/duplicate guards, bracketed payload, newer drafts and trailing key editor');
 const historyRoot=new Element('div'),historyHandle=context.mountFixture(historyRoot,711);find(historyRoot,'terminal-scroll').clientHeight=400;await flush();const historySocket=context.sockets.at(-1);historySocket.handshake();historySocket.screen();await flush();
 const request=historySocket.sent.find(a=>a[4]===0x12),hr=T.reader(request.slice(5));hr.n();assert.equal(hr.n(),0);assert.equal(hr.n(),0);assert.equal(hr.n(),300);historyHandle.unmount();
 console.log('PASS visible terminal preloads 300 earlier rows on first screen');
 const A=await mount(901,{stateKey:'host-A\n901',preferences:{inputMode:'buffered'}}),commandA=find(A.root,'command-input');commandA.value='HOST_A_PRIVATE_DRAFT';event(commandA,'input');A.handle.unmount();
 const B=await mount(901,{stateKey:'host-B\n901'});assert.equal(find(B.root,'command-input').value,'');assert.equal(B.handle.getInputMode(),'direct');B.handle.unmount();
 const restoredA=await mount(901,{stateKey:'host-A\n901'});assert.equal(find(restoredA.root,'command-input').value,'HOST_A_PRIVATE_DRAFT');assert.equal(restoredA.handle.getInputMode(),'buffered');assert(context.KrillMobile.terminal.snapshotDrafts().some(([key,text])=>key==='host-A\n901'&&text==='HOST_A_PRIVATE_DRAFT'));assert.equal(inputs(restoredA.socket).length,0);restoredA.handle.unmount();
 console.log('PASS host-scoped terminal drafts/modes restore without input replay for identical session IDs');}
}
if(process.argv.includes('--harness')){
 const directory='/tmp/mk-mobile/term';fs.mkdirSync(directory,{recursive:true});fs.writeFileSync(path.join(directory,'fixture.js'),`(${fixture})();\n${source}\n globalThis.handle=mountFixture(document.querySelector('main'));setTimeout(()=>{sockets[0].handshake();sockets[0].screen('~/krill  git: mobile-orca-ui');sockets[0].autoAck=true;},0);`);
 fs.writeFileSync(path.join(directory,'term-harness.html'),'<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover"><link rel="stylesheet" href="/app.css"><main class="terminal-mount"></main><script src="/fixture.js"></script>');
 console.log('Harness written to '+directory);
}else{const watchdog=setTimeout(()=>{console.error('Terminal checks did not settle');process.exitCode=1;},5000);main().catch(e=>{console.error(e);process.exitCode=1;}).finally(()=>clearTimeout(watchdog));}
