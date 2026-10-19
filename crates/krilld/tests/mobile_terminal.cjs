// Standalone terminal adapter regression harness: node crates/krilld/tests/mobile_terminal.cjs
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/terminal.js'),'utf8');
class Element extends EventTarget {
 constructor(tag){super();this.tagName=tag;this.children=[];this.dataset={};this.attrs={};this.style={setProperty(k,v){this[k]=v;},removeProperty(k){delete this[k];}};this.classList={add(){},remove(){}};this.value='';this.clientWidth=390;this.scrollTop=0;this.scrollHeight=0;this.namespaceURI='http://www.w3.org/2000/svg';}
 get textContent(){return this.children.length?this.children.map(c=>typeof c==='string'?c:c.textContent).join(''):this.text??'';}
 set textContent(v){this.children=[];this.text=v;}
 append(...nodes){this.children.push(...nodes);for(const n of nodes)if(typeof n==='object')n.parent=this;}
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
  handshake(){const T=MkTerminalCodec;this.message(JSON.stringify({version:2,challenge:''}));this.message(new TextEncoder().encode('{}'));this.frame(1,T.v(1),T.v(1),T.v((1n<<56n)|4096n|5n));this.frame(0x2c,T.v(0));}
  screen(text='Krill · live terminal'){const T=MkTerminalCodec,C=MkCrypto,chars=[...text],cell=ch=>C.concat(T.text(ch),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0));this.frame(0x21,T.v(1),T.v(1),T.v(1),T.v(4),T.v(0),T.v(80),T.v(24),T.v(2),T.v(0),T.v(chars.length),...chars.map(cell),T.v(3),T.v(1),T.v(2),T.v(1),T.v(4),T.text('Terminal #7'));}
 };
 globalThis.mountFixture=(root,sessionId=7,extra={})=>KrillMobile.terminal.mount(root,{sessionId,connect:()=>({socket:new FakeSocket(),credentials:{device_id:'fixture',credential:''}}),...extra});
}
const viewport=Object.assign(new EventTarget(),{scale:1,height:844,offsetTop:0});
const globalEvents=new EventTarget(),storage=new Map(),timers=new Map();let clock=0,nextTimer=0;
const context=vm.createContext({Event,EventTarget,AbortController,TextEncoder,TextDecoder,Uint8Array,DataView,Map,Set,Promise,console,queueMicrotask,crypto:require('node:crypto').webcrypto,innerHeight:844,devicePixelRatio:1,visualViewport:viewport,document:{createElement:tag=>new Element(tag),createElementNS:(_,tag)=>new Element(tag)},localStorage:{getItem:k=>storage.get(k),setItem:(k,v)=>storage.set(k,v)},addEventListener:globalEvents.addEventListener.bind(globalEvents),setTimeout:(fn,delay)=>{const id=++nextTimer;timers.set(id,{fn,at:clock+delay});return id;},clearTimeout:id=>timers.delete(id)});
vm.runInContext(`(${fixture})()`,context);vm.runInContext(source,context);
const api=context.KrillMobile.terminal,T=context.MkTerminalCodec,C=context.MkCrypto;
const event=(el,type,props={})=>{const e=new Event(type,{cancelable:true});Object.assign(e,props);el.dispatchEvent(e);return e;};
const find=(root,cls)=>{if(root.className?.split(' ').includes(cls))return root;for(const c of root.children){const found=find(c,cls);if(found)return found;}};
const key=(root,id)=>find(root,'keybar').children.find(el=>el.dataset.keyId===id);
const flush=async()=>{for(let i=0;i<8;i++)await Promise.resolve();};
async function advance(ms){const end=clock+ms;for(;;){const next=[...timers].filter(([,t])=>t.at<=end).sort((a,b)=>a[1].at-b[1].at)[0];if(!next)break;clock=next[1].at;timers.delete(next[0]);next[1].fn();await flush();}clock=end;await flush();}
const inputs=socket=>Array.from(socket.sent).filter(a=>a instanceof Uint8Array&&a[4]===0x13).map(a=>{const r=T.reader(a.slice(5));r.n();return r.text();});
async function mount(id=7,extra={}){const root=new Element('div'),handle=context.mountFixture(root,id,extra);await flush();const socket=context.sockets.at(-1);socket.handshake();socket.screen();await flush();return {root,handle,socket};}
async function main(){
 assert(!source.includes('innerHTML'));assert(!source.includes('getElementById'));assert.equal(api.keys.length,20);
 const expected=['\x1b','\t','\r','\x1b[Z',' ','\x7f','\x1b[3~','\x1b[A','\x1b[B','\x1b[D','\x1b[C','\x03','\x04','\x0c','\x1a','\x12','\x01','\x05','\x17','\x15'];
 let {root,handle,socket}=await mount();socket.autoAck=true;
 for(const k of api.keys)await handle.sendKey(k.id);assert.deepEqual(inputs(socket),expected);console.log('PASS exact 20-key order and bytes');
 viewport.height=544;event(viewport,'resize');assert.equal(root.style['--keyboard-inset'],'300px');assert.equal(find(root,'dismiss-keyboard').hidden,false);viewport.scale=1.5;event(viewport,'resize');assert.equal(root.style['--keyboard-inset'],'0px');assert.equal(find(root,'dismiss-keyboard').hidden,true);viewport.scale=1;viewport.height=844;event(viewport,'resize');console.log('PASS visualViewport keyboard inset excludes browser zoom');
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
 capture.value='a';event(capture,'input');assert.equal(event(capture,'keydown',{key:'Backspace'}).defaultPrevented,false);await flush();console.log('PASS iOS-style replace-in-place Hangul keeps textarea context, diff sends DEL+syllable, control keys reset context');
 count=inputs(socket).length;event(capture,'compositionstart');handle.setInputMode('buffered');handle.setInputMode('direct');event(capture,'compositionend',{data:'늦음'});capture.value='늦음';event(capture,'input');await flush();assert.equal(inputs(socket).length,count);assert.equal(capture.value,'');
 handle.focus();event(capture,'compositionstart');handle.unmount();event(capture,'compositionend',{data:'이전'});capture.value='이전';event(capture,'input');await flush();assert.equal(inputs(socket).length,count);assert.equal(root.children.length,0);console.log('PASS generation discard on mode switch / unmount');
 ({root,handle,socket}=await mount());handle.setInputMode('buffered');let command=find(root,'command-input');command.value='echo 한글';event(command,'input');event(find(root,'buffered-command'),'submit');await flush();assert.equal(command.value,'echo 한글');let sent=socket.sent.at(-1),r=T.reader(sent.slice(5)),id=r.n();socket.frame(0x27,T.v(id),T.v(0));await flush();assert.equal(command.value,'');
 command.value='saved draft';event(command,'input');handle.unmount();({root,handle,socket}=await mount());assert.equal(handle.getInputMode(),'buffered');assert.equal(find(root,'command-input').value,'saved draft');console.log('PASS buffered draft retention, opt-out and ACK-only clear');
 let fit=handle.fitToPhone();assert.equal(find(root,'display-mode').getAttribute('aria-pressed'),'false');socket.frame(0x2c,T.v(1));await assert.rejects(fit,/rejected/);assert.equal(find(root,'display-mode').getAttribute('aria-pressed'),'false');fit=handle.fitToPhone();socket.frame(0x2c,T.v(0));await fit;assert.equal(find(root,'display-mode').getAttribute('aria-pressed'),'true');fit=handle.fitToPhone();socket.frame(0x2c,T.v(0));await fit;
 count=socket.sent.filter(a=>a[4]===0x17).length;handle.setFontScale(1.4);assert.equal(socket.sent.filter(a=>a[4]===0x17).length,count);assert.equal(find(root,'terminal-history').style.fontSize,'20px');console.log('PASS Fit ownership ACK, failure rollback and local font sizing');
 handle.loadHistory();sent=socket.sent.at(-1);r=T.reader(sent.slice(5));const req=r.n();assert.equal(r.n(),0);assert.equal(r.n(),0);assert.equal(r.n(),100);const rows=Array.from({length:100},()=>[{ch:'x',zero:'',flags:0}]);const row=C.concat(T.v(1),T.text('x'),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0));socket.frame(0x26,T.v(req),T.v(100),T.v(2),T.v(0),T.v(100),...Array(100).fill(row));handle.loadHistory();r=T.reader(socket.sent.at(-1).slice(5));r.n();assert.equal(r.n(),1);assert.equal(r.n(),1);let historyReq;for(let page=0;page<9;page++){r=T.reader(socket.sent.at(-1).slice(5));historyReq=r.n();socket.frame(0x26,T.v(historyReq),T.v(10000),T.v(2),T.v(page*100),T.v(100),...Array(100).fill(row));handle.loadHistory();}assert.equal(find(root,'terminal-history').textContent.split('\n').length,1001);assert.equal(find(root,'load-history').disabled,true);const before=socket.sent.length;handle.loadHistory();assert.equal(socket.sent.length,before);console.log('PASS 100-row hot-to-cold history and 1000-row cap');
 ({root,handle,socket}=await mount(46));handle.loadHistory();r=T.reader(socket.sent.at(-1).slice(5));const wreq=r.n();const wc=(ch,zero,flags)=>C.concat(T.text(ch),T.v(zero.length),...zero.map(z=>T.text(z)),T.v(0),T.v(0),T.v(0),T.v(flags));
 socket.frame(0x26,T.v(wreq),T.v(1),T.v(1),T.v(0),T.v(1),C.concat(T.v(6),wc('a',[],0),wc('ᄒ',['ᅡ','ᆫ'],8),wc(' ',[],16),wc('글',[],8),wc(' ',[],16),wc('b',[],0)));
 const hist=find(root,'terminal-history'),span=hist.children.find(c=>typeof c==='object');assert.equal(hist.textContent,'a한글b\n');assert.equal(span.textContent,'한글');assert.equal(span.style.letterSpacing,'var(--wide-spacing)');assert.match(handle.text(),/^a한글b/);handle.unmount();console.log('PASS history <pre>: NFD joined to NFC, wide runs spaced to two cells');
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
 event(port,'pointerdown',{clientX:100,clientY:100});event(port,'pointermove',{clientX:104,clientY:103});event(port,'click');assert.equal(focused,1);
 event(port,'pointerdown',{clientX:100,clientY:100});event(port,'pointermove',{clientX:100,clientY:130});event(port,'click');assert.equal(focused,1);
 port.scrollLeft=50;const touch=(type,x,y)=>event(port,type,{touches:x==null?[]:[{clientX:x,clientY:y}]});
 touch('touchstart',200,400);let te=touch('touchmove',190,330);assert.equal(te.defaultPrevented,false);touch('touchmove',150,200);assert.equal(port.scrollLeft,50);touch('touchend');
 touch('touchstart',200,400);te=touch('touchmove',190,398);assert(te.defaultPrevented);te=touch('touchmove',120,390);assert(te.defaultPrevented);assert.equal(port.scrollLeft,130);touch('touchend');
 te=event(port,'wheel',{deltaX:30,deltaY:2});assert(te.defaultPrevented);assert.equal(port.scrollLeft,160);event(port,'wheel',{deltaX:1,deltaY:40});assert.equal(port.scrollLeft,160);
 socket.autoAck=true;for(let i=0;i<5;i++){socket.screen(`flow ${i} `+'w'.repeat(70));await advance(16);}await handle.sendKey('enter');assert.equal(port.scrollLeft,160);
 assert.match(handle.text(),/flow 4 w+$/);
 context.document.activeElement=capture2;handle.setInputMode('buffered');assert.equal(commandFocused,1);context.document.activeElement=null;handle.setInputMode('direct');assert.equal(focused,1);
 socket.close();assert.equal(root.dataset.phase,'disconnected');handle.unmount();assert.equal(root.dataset.phase,undefined);
 ({root,handle,socket}=await mount(43));let statuses=[];handle.unmount();({root,handle,socket}=await mount(44,{onStatus:st=>statuses.push(st)}));context.document.activeElement=find(root,'live-capture');socket.close();assert.equal(statuses.at(-1).typing,true);context.document.activeElement=null;handle.unmount();
 ({root,handle,socket}=await mount(45));let fitting=handle.fitToPhone();socket.frame(0x2c,T.v(0));await fitting;const roles=()=>socket.sent.filter(a=>a[4]===0x17).length;let before17=roles();socket.frame(0x21,T.v(3),T.v(3),T.v(1),T.v(1),C.concat(T.v(0),T.v(48),T.v(24)));await flush();viewport.offsetTop=5;event(viewport,'scroll');viewport.offsetTop=0;await flush();assert.equal(roles(),before17);handle.unmount();
 console.log('PASS disconnect reports typing for refocus, Fit skips same-column resize roles');
 console.log('PASS tap jitter focus, one-axis touch pan (no native horizontal), output/input keep scrollLeft, mode-switch refocus, text() and phase');
 console.log('All terminal adapter regression checks passed.');
}
if(process.argv.includes('--harness')){
 const directory='/tmp/mk-mobile/term';fs.mkdirSync(directory,{recursive:true});fs.writeFileSync(path.join(directory,'fixture.js'),`(${fixture})();\n${source}\n globalThis.handle=mountFixture(document.querySelector('main'));setTimeout(()=>{sockets[0].handshake();sockets[0].screen('~/krill  git: mobile-orca-ui');sockets[0].autoAck=true;},0);`);
 fs.writeFileSync(path.join(directory,'term-harness.html'),'<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover"><link rel="stylesheet" href="/app.css"><main class="terminal-mount"></main><script src="/fixture.js"></script>');
 console.log('Harness written to '+directory);
}else{const watchdog=setTimeout(()=>{console.error('Terminal checks did not settle');process.exitCode=1;},5000);main().catch(e=>{console.error(e);process.exitCode=1;}).finally(()=>clearTimeout(watchdog));}
