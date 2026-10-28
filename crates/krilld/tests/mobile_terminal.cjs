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
 replaceWith(node){const p=this.parent;if(!p)return;p.children[p.children.indexOf(this)]=node;node.parent=p;}
 focus(){this.dispatchEvent(new Event('focus'));}
 blur(){this.dispatchEvent(new Event('blur'));}
 getContext(){const calls=this.calls={fillText:0,fillRect:0,font:0,draws:0};return new Proxy({},{get:(_,k)=>k==='fillText'?t=>{calls.fillText++;calls.texts?.push(t);}:k==='fillRect'?()=>{calls.fillRect++;}:k==='setTransform'?()=>{calls.draws++;}:()=>{},set:(_,k)=>{if(k==='font')calls.font++;return true;}});}
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
  question(lines,done=true){const T=MkTerminalCodec,C=MkCrypto,cell=ch=>C.concat(T.text(ch),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0)),rows=lines.map((text,y)=>C.concat(T.v(2),T.v(y),T.v([...text].length),...[...text].map(cell)));this.frame(0x21,T.v(1),T.v(1),T.v(done?1:0),T.v(rows.length+1),T.v(0),T.v(80),T.v(24),...rows);}
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
async function testInsertText(){
 const {root,handle,socket}=await mount(910,{stateKey:'quick-host:910',preferences:{fitToPhone:false,inputMode:'direct'}});socket.autoAck=true;
 assert.equal(handle.insertText(null),false);assert.equal(handle.insertText('\r\n\x1b\r\n'),false);
 assert.equal(handle.insertText('\r\n/krill-orchestration \r\n'),true);await flush();assert.equal(inputs(socket).at(-1),'/krill-orchestration ');assert(!inputs(socket).at(-1).includes('\r'));
 handle.setInputMode('buffered');const command=find(root,'command-input'),before=inputs(socket).length;
 assert.equal(handle.insertText('/ship-dev'),true);assert.equal(command.value,'/ship-dev');assert.equal(command.selectionStart,9);assert.equal(new Map(api.snapshotDrafts()).get('quick-host:910'),'/ship-dev');
 command.value='git OLD --short';command.selectionStart=4;command.selectionEnd=7;handle.insertText('status');assert.equal(command.value,'git status --short');assert.equal(command.selectionEnd,10);
 command.selectionStart=command.selectionEnd=undefined;handle.insertText(' ');assert.equal(command.value,'git status --short ');await flush();assert.equal(inputs(socket).length,before,'buffered insertion never submits');handle.unmount();
 console.log('PASS saved text insertion: direct PTY bytes omit Enter, trailing space kept, buffered selection/cursor and host draft updated without send');
}
async function testLivePreview(){
 const {root,handle,socket}=await mount(1951,{preferences:{inputMode:'direct',fitToPhone:false}}),capture=find(root,'live-capture'),live=find(root,'live-input-copy'),title=live.children[0],detail=live.children[1],keybar=find(root,'keybar');handle.focus();
 assert.deepEqual(keybar.children.slice(3,14).map(b=>b.dataset.keyId),['escape','tab','enter','shiftTab','space','backspace','delete','arrowUp','arrowDown','arrowLeft','arrowRight']);
 assert.equal(keybar.children[0].children[0].tagName,'svg');assert.equal(keybar.children[0].getAttribute('aria-pressed'),'true');
 assert.deepEqual(keybar.children.slice(-3).map(b=>b.dataset.keyId),['shift','ctrl','alt']);assert.equal(capture.parent,find(root,'accessory-row'));
 capture.value='hello';event(capture,'input');assert.equal(detail.textContent,'hello');assert.deepEqual(inputs(socket),['hello']);
 await advance(1000);assert.equal(root.dataset.phase,'input-pending');assert.match(title.textContent,/Waiting for input/);assert.equal(detail.textContent,'hello');
 socket.screen('output while ACK waits');assert.equal(detail.textContent,'hello');socket.frame(0x27,T.v(1),T.v(0));await flush();assert.equal(title.textContent,'Live input');assert.equal(detail.textContent,'hello');
 event(viewport,'resize');await advance(16);assert.equal(detail.textContent,'hello','Viewport refresh never overwrites preview');
 event(capture,'compositionstart');capture.value='helloㅎ';event(capture,'input',{isComposing:true});assert.equal(detail.textContent,'helloㅎ');assert.deepEqual(inputs(socket),['hello']);
 capture.value='hello한';event(capture,'input',{isComposing:true});assert.equal(detail.textContent,'hello한');event(capture,'compositionend',{data:'한'});event(capture,'input',{inputType:'insertFromComposition'});await flush();assert.deepEqual(inputs(socket),['hello','한']);assert.equal(detail.textContent,'hello한');
 socket.autoAck=true;capture.value='hello';event(capture,'input',{inputType:'deleteContentBackward'});await flush();assert.deepEqual(inputs(socket),['hello','한','\x7f']);assert.equal(detail.textContent,'hello');
 event(capture,'keydown',{key:'Enter'});await flush();assert.equal(inputs(socket).at(-1),'\r');assert.equal(detail.dataset.preview,'false');assert.equal(capture.value,'');
 handle.setInputMode('buffered');assert.equal(keybar.children[0].children[0].tagName,'svg');assert.equal(keybar.children[0].getAttribute('aria-pressed'),'false');handle.setInputMode('direct');handle.focus();capture.value='not replayed';event(capture,'input');socket.close();await flush();assert.equal(detail.dataset.preview,'false');handle.unmount();
 const again=await mount(1951,{preferences:{inputMode:'direct',fitToPhone:false}});assert.deepEqual(inputs(again.socket),[]);assert.equal(find(again.root,'live-input-copy').children[1].dataset.preview,'false');again.handle.unmount();
 console.log('PASS Orca dock order/icon, stable live preview through delayed ACK/output/viewport, IME no duplicate commit, backspace, Enter and reconnect no replay');
}
async function testPageZoom(){
 const doc=context.document;
 for(const type of ['touchstart','touchmove']){
  assert.equal(event(doc,type,{touches:[{}]}).defaultPrevented,false,'one finger keeps native scrolling');
  assert.equal(event(doc,type,{touches:[{},{}]}).defaultPrevented,true,'two fingers cannot zoom the page outside the terminal');
 }
 for(const type of ['gesturestart','gesturechange'])assert.equal(event(doc,type).defaultPrevented,true,'Safari native page zoom is cancelled');
 let scale;const {root,handle}=await mount(1952,{preferences:{fitToPhone:false},onPreferenceChange:p=>{scale=p.textScale;}}),port=find(root,'terminal-scroll'),dock=find(root,'command-dock');
 const touches=distance=>[{clientX:100,clientY:100},{clientX:100+distance,clientY:100}];
 // The document capture guard must leave the terminal's own listeners reachable.
 const start=new Event('touchstart',{cancelable:true});start.touches=touches(100);doc.dispatchEvent(start);port.dispatchEvent(start);
 const move=new Event('touchmove',{cancelable:true});move.touches=touches(150);doc.dispatchEvent(move);port.dispatchEvent(move);
 assert(move.defaultPrevented);event(port,'touchend',{touches:[]});assert.equal(scale,1.5,'pinch still changes only terminal text');assert.equal(dock.style.transform,undefined);handle.unmount();
 console.log('PASS page/Safari zoom blocked, single-finger scroll retained, terminal pinch still changes text only');
}
async function testQuestions(){
 const {root,handle,socket}=await mount(1960,{preferences:{fitToPhone:false}}),panel=find(root,'terminal-question');
 const choices=()=>panel.children.filter(n=>n.tagName==='button'),menu=['Which approach?', '› 1. Use the existing service (Recommended)', '     Keeps current credentials', '  2. Create a new service', '  3. 직접 입력', '     다른 방법을 설명합니다', 'Press enter to confirm or esc to cancel'];
 assert.equal(panel.hidden,true);socket.question(menu);assert.equal(panel.hidden,false);assert.equal(choices().length,3);assert.match(choices()[0].textContent,/Keeps current credentials/);assert.match(choices()[2].textContent,/다른 방법/);
 let focused=0;find(root,'live-capture').addEventListener('focus',()=>focused++);const old=choices()[2];event(old,'click');event(old,'click');await flush();assert.deepEqual(inputs(socket),['\x1b[B\x1b[B\r']);assert(choices().every(b=>b.disabled));assert.equal(focused,0,'answers do not open the software keyboard');
 socket.frame(0x27,T.v(1),T.v(0));await flush();event(old,'click');assert.equal(inputs(socket).length,1,'ACK alone cannot resubmit the same visible question');
 socket.question(['Apply the change?', '  1. Yes', '  2. Always allow', '❯ 3. No', 'Enter to select · Esc to cancel']);event(old,'click');assert.equal(inputs(socket).length,1,'detached answer from a previous question is inert');
 socket.autoAck=true;event(choices()[0],'click');await flush();assert.equal(inputs(socket).at(-1),'\x1b[A\x1b[A\r');
 socket.frame(0x2b,Uint8Array.of(0),T.v(1));socket.question(['Choose a plan', '❯ 1. Basic', '  2. Advanced', 'Enter to select']);event(choices()[1],'click');await flush();assert.equal(inputs(socket).at(-1),'\x1bOB\r','application cursor mode uses SS3');
 socket.question(['Pick features', '❯ ☐ Search', '  ☑ Export', 'Space to select · Enter to submit']);assert.equal(choices().length,3);assert.equal(choices()[0].getAttribute('aria-checked'),'false');assert.equal(choices()[1].getAttribute('aria-checked'),'true');event(choices()[1],'click');await flush();assert.equal(inputs(socket).at(-1),'\x1bOB ');assert(choices().every(b=>b.disabled));
 socket.question(['Pick features', '  ☐ Search', '❯ ☐ Export', 'Space to select · Enter to submit']);assert.equal(choices()[1].getAttribute('aria-checked'),'false');event(choices().at(-1),'click');await flush();assert.equal(inputs(socket).at(-1),'\r','multi-select submits only with the explicit button');
 const invalid=[['1. Documentation','2. More documentation','Enter to select'],['❯ 1. Yes','2. No'],['❯ 1. Yes','3. No','Enter to select'],['❯ 1. Yes','2. No','Space to select · Enter to submit'],['❯ 1. Yes','2. No','Enter to select','$ echo done'],['❯ 1. [ ] Yes','2. No','Enter to submit'],['❯ 1. Only','Enter to select'],['❯ 1. First',...Array.from({length:12},(_,i)=>`${i+2}. Option`),'Enter to select']];
 for(const lines of invalid){socket.question(lines);assert.equal(panel.hidden,true,lines.join('\n'));}
 socket.question(menu);const prior=choices()[1],before=inputs(socket).length;socket.question(menu,false);assert.equal(panel.hidden,true);event(prior,'click');assert.equal(inputs(socket).length,before,'partial screen frames cannot answer');socket.question(menu);
 handle.setInputActive(false);event(prior,'click');assert.equal(inputs(socket).length,before);handle.setInputActive(true);event(prior,'click');assert.equal(inputs(socket).length,before,'old input generation is inert');
 const fresh=choices()[1];context.document.visibilityState='hidden';event(fresh,'click');assert.equal(inputs(socket).length,before);context.document.visibilityState='visible';
 const port=find(root,'terminal-scroll');port.clientHeight=400;port.scrollHeight=1000;port.scrollTop=100;event(port,'scroll');assert.equal(panel.hidden,true);event(fresh,'click');assert.equal(inputs(socket).length,before);event(find(root,'terminal-latest'),'click');assert.equal(panel.hidden,false);
 const capture=find(root,'live-capture');handle.focus();event(capture,'compositionstart');event(choices()[0],'click');assert.equal(inputs(socket).length,before,'IME composition is never interrupted by answer buttons');event(capture,'compositionend',{data:''});await flush();
 socket.autoAck=false;const pending=handle.sendKey('arrowDown');assert(choices().every(b=>b.disabled));event(choices()[0],'click');assert.equal(inputs(socket).length,before+1);socket.frame(0x27,T.v(before+1),T.v(0));await pending;
 // Deterministic malformed-menu fuzz: bounded wire input, no throws or actions.
 let seed=17;for(let n=0;n<100;n++){seed=(seed*1664525+1013904223)>>>0;const junk=Array.from({length:seed%12},(_,i)=>`${['', '❯ ', '☐ ', '> '][(seed>>>i)%4]}${i+2}${['.', ')', ':'][seed%3]} ${String.fromCodePoint(0xAC00+seed%1000)} <img>`);socket.question(junk);assert.equal(panel.hidden,true);}
 socket.question(menu);socket.close();event(choices()[0],'click');assert.equal(inputs(socket).length,before+1);assert.equal(panel.hidden,true);handle.unmount();
 console.log('PASS live question touch answers: Claude/Codex arrows+Enter, wrapped labels, SS3, checkboxes+submit, duplicate/stale/partial/history/hidden/IME/pending/disconnect guards and malformed-menu fuzz');
}
async function main(){
 await testInsertText();
 await testQuestions();
 await testPageZoom();
 await testHiddenPaint();
 await testLivePreview();
 for(const n of [0,1,127,128,300,65535,Number.MAX_SAFE_INTEGER]){const r=T.reader(T.v(n));assert.equal(r.n(),n);r.end();}assert.throws(()=>T.reader(T.v(128)).n(127),/Screen limit/);assert.throws(()=>T.reader(T.v(128)).n(128-1),/Screen limit/);assert.throws(()=>T.reader(T.v(1n<<53n)).n(),/Screen limit/);assert.throws(()=>T.reader(Uint8Array.of(128)).n(),/Truncated/);
 for(const text of ['', 'a', '\0', '한글', 'é', '😀']){const r=T.reader(T.text(text));assert.equal(r.text(),text);r.end();}assert.throws(()=>T.reader(Uint8Array.of(1,255)).text());assert.throws(()=>T.reader(Uint8Array.of(2,97)).text(),/Truncated/);
 assert(!source.includes('innerHTML'));assert(!source.includes('getElementById'));assert.equal(api.keys.length,24);
 const expected=['\x1b','\t','\r','\x1b[Z',' ','\x7f','\x1b[3~','\x1b[A','\x1b[B','\x1b[D','\x1b[C','\x03','\x04','\x0c','\x1a','\x12','\x01','\x05','\x17','\x15','\x1b[H','\x1b[F','\x1b[5~','\x1b[6~'];
 const quickFixture=await mount(711),dockKeys=find(quickFixture.root,'keybar');
 assert.deepEqual(dockKeys.children.slice(3,14).map(b=>b.dataset.keyId),['escape','tab','enter','shiftTab','space','backspace','delete','arrowUp','arrowDown','arrowLeft','arrowRight']);
 for(const name of ['quick-actions','dismiss-keyboard'])assert.equal(find(quickFixture.root,name),undefined,name+' is absent from the dock');
 for(const name of ['paste-key','display-mode'])assert.equal(find(quickFixture.root,name).parent,dockKeys,name+' scrolls inside the terminal key list');
 assert.deepEqual(inputs(quickFixture.socket),[],'building the dock never sends input');quickFixture.handle.unmount();
 console.log('PASS priority keys first and paste/display actions inside the scrolling key list');
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
  viewport.height=700;event(viewport,'resize');viewport.height=544;event(viewport,'resize');event(viewport,'scroll');assert.deepEqual(writes,[],'keyboard events only schedule; no layout write before the frame');await advance(16);assert.deepEqual(writes,[['--keyboard-inset','300px']],'one CSS variable write per frame on <html>');assert.equal(root.style['--keyboard-inset'],undefined,'terminal does not keep its own inset');assert.equal(find(root,'show-keyboard').getAttribute('aria-pressed'),'true');assert.equal(root.dataset.keyboardOpen,'true');
  viewport.scale=1.5;event(viewport,'resize');await advance(16);assert.deepEqual(writes.at(-1),['--keyboard-inset','0px'],'pinch zoom is not a keyboard');assert.equal(find(root,'show-keyboard').getAttribute('aria-pressed'),'true','zoom does not blur the focused input');viewport.scale=undefined;event(viewport,'resize');await advance(16);assert.deepEqual(writes.at(-1),['--keyboard-inset','300px'],'a WebView without scale is unzoomed');
  const scrolls=[];context.scrollY=40;context.scrollTo=(x,y)=>{scrolls.push([x,y]);context.scrollY=0;};context.document.activeElement={closest:s=>s.includes('.command-dock')?{}:null};viewport.offsetTop=40;viewport.height=504;event(viewport,'scroll');await advance(16);assert.deepEqual(scrolls,[[0,0]],'iOS focus scroll is put back for the docked input');viewport.offsetTop=0;viewport.height=544;event(viewport,'scroll');await advance(16);assert.equal(scrolls.length,1);assert.equal(writes.length,3,'unchanged inset is not rewritten');
  context.document.activeElement=null;viewport.scale=1;viewport.height=844;event(viewport,'resize');await advance(16);assert.deepEqual(writes.at(-1),['--keyboard-inset','0px']);html.style.setProperty=set;}
 {const prevented=[],container={addEventListener(type,fn,opts){assert.equal(type,'mousedown');assert.equal(opts.capture,true);this.fn=fn;},contains:()=>true},button={},tap=target=>{const e={target,preventDefault(){prevented.push(target);}};container.fn(e);};context.MkKeyboard.keepFocus(container);const onButton={closest:s=>s==='button'?button:null,matches:()=>false},onText={closest:()=>null,matches:()=>true};
  context.document.activeElement={matches:()=>true};tap(onButton);tap(onText);context.document.activeElement={matches:()=>false};tap(onButton);context.document.activeElement=null;assert.deepEqual(prevented,[onButton],'a button tap keeps the text field focused only while typing');}
 console.log('PASS visualViewport keyboard inset: one rAF-batched <html> write, pinch zoom ignored, iOS focus scroll reset, send taps keep the keyboard');
 const attach=socket.sent.find(a=>a[4]===0x10),role=socket.sent.find(a=>a[4]===0x17);assert.deepEqual([...attach.slice(-2)],[0,0]);assert.deepEqual([...role.slice(5)],[0,2,0]);
 let count=inputs(socket).length;event(key(root,'backspace'),'pointerdown',{button:0,pointerId:1});await flush();assert.equal(inputs(socket).length,count+1);await advance(399);assert.equal(inputs(socket).length,count+1);await advance(1);assert.equal(inputs(socket).length,count+2);await advance(45);assert.equal(inputs(socket).length,count+3);event(key(root,'backspace'),'pointerup');await advance(500);assert.equal(inputs(socket).length,count+3);console.log('PASS repeat immediate / 400ms / 45ms and release');
 const capture=find(root,'live-capture');handle.focus();count=inputs(socket).length;event(capture,'compositionstart');capture.value='ㅎ';event(capture,'input',{isComposing:true});capture.value='한';event(capture,'input',{isComposing:true});assert.equal(inputs(socket).length,count);event(capture,'compositionend',{data:'한'});capture.value='한';event(capture,'input',{isComposing:false});await flush();assert.deepEqual(inputs(socket).slice(count),['한']);
 event(capture,'compositionstart');capture.value='한글';const pending=handle.sendKey('enter');event(capture,'compositionend',{data:'글'});await advance(0);await pending;handle.focus();assert.deepEqual(inputs(socket).slice(-2),['글','\r']);console.log('PASS Korean IME commit once and control-key ordering');
 // iOS 한글 키보드 모델: 조합 이벤트 없이 앞 음절을 지우고(deleteContentBackward) 새 음절을 넣는다. textarea를 비우지 않아야 다음 자모가 앞 글자와 합쳐진다.
 count=inputs(socket).length;handle.sendKey('escape');count++;const type=(value,inputType)=>{const before=event(capture,'beforeinput',{inputType});if(!before.defaultPrevented)capture.value=value;event(capture,'input',{inputType});return before;};
 for(const [value,inputType] of [['ㅎ','insertText'],['','deleteContentBackward'],['하','insertText'],['','deleteContentBackward'],['한','insertText'],['한ㄱ','insertText'],['한','deleteContentBackward'],['한그','insertText'],['한','deleteContentBackward'],['한글','insertText']]){assert.equal(type(value,inputType).defaultPrevented,false);assert.equal(capture.value,value);}
 await flush();assert.deepEqual(inputs(socket).slice(count),['한']);assert.equal(find(root,'terminal-preedit').textContent,'글');
 count=inputs(socket).length;capture.value='한글!';event(capture,'input');capture.value='한글?';event(capture,'input');await flush();assert.deepEqual(inputs(socket).slice(count),['글!','\x7f?']);
 count=inputs(socket).length;await handle.sendKey('enter');assert.equal(capture.value,'');assert(type('','deleteContentBackward').defaultPrevented);event(capture,'keydown',{key:'Backspace'});await flush();assert.deepEqual(inputs(socket).slice(count),['\r','\x7f','\x7f']);
 capture.value='a';event(capture,'input');assert.equal(event(capture,'keydown',{key:'Backspace'}).defaultPrevented,false);await flush();count=inputs(socket).length;handle.toggleShift(true);capture.value='ab';event(capture,'input');await flush();assert.deepEqual(inputs(socket).slice(count),['B']);console.log('PASS eventless WebKit Hangul keeps native context, holds tail without DEL repair, controls flush once');
 count=inputs(socket).length;event(capture,'compositionstart');handle.setInputMode('buffered');handle.setInputMode('direct');event(capture,'compositionend',{data:'늦음'});capture.value='늦음';event(capture,'input');await flush();assert.equal(inputs(socket).length,count);assert.equal(capture.value,'');
 handle.focus();event(capture,'compositionstart');handle.unmount();event(capture,'compositionend',{data:'이전'});capture.value='이전';event(capture,'input');await flush();assert.equal(inputs(socket).length,count);assert.equal(root.children.length,0);console.log('PASS generation discard on mode switch / unmount');
 ({root,handle,socket}=await mount());handle.setInputMode('buffered');let command=find(root,'command-input');command.value='echo 한글';event(command,'input');event(find(root,'buffered-command'),'submit');await flush();assert.equal(command.value,'echo 한글');let sent=socket.sent.at(-1),r=T.reader(sent.slice(5)),id=r.n();socket.frame(0x27,T.v(id),T.v(0));await flush();assert.equal(command.value,'');
 command.value='saved draft';event(command,'input');handle.unmount();({root,handle,socket}=await mount());assert.equal(handle.getInputMode(),'buffered');assert.equal(find(root,'command-input').value,'saved draft');const mode=find(root,'direct-toggle');assert.equal(mode.children[0].tagName,'svg');assert.equal(mode.attrs?.['aria-label']??mode.getAttribute?.('aria-label'),'Buffered input');handle.setInputMode('direct');assert.equal(mode.getAttribute('aria-pressed'),'true');console.log('PASS buffered draft retention, opt-out and ACK-only clear');
 let fit=handle.fitToPhone();assert.equal(find(root,'display-mode').getAttribute('aria-label'),'Fit to Phone');socket.frame(0x2c,T.v(1));await assert.rejects(fit,/rejected/);assert.equal(find(root,'display-mode').getAttribute('aria-label'),'Fit to Phone');fit=handle.fitToPhone();socket.frame(0x2c,T.v(0));await fit;assert.equal(find(root,'display-mode').getAttribute('aria-label'),'Match Desktop');fit=handle.fitToPhone();socket.frame(0x2c,T.v(0));await fit;
 count=socket.sent.filter(a=>a[4]===0x17).length;handle.setFontScale(1.4);assert.equal(socket.sent.filter(a=>a[4]===0x17).length,count);assert.equal(find(root,'terminal-history').style.fontSize,'20px');console.log('PASS Fit ownership ACK, failure rollback and local font sizing');
 handle.loadHistory();sent=socket.sent.at(-1);r=T.reader(sent.slice(5));const req=r.n();assert.equal(r.n(),0);assert.equal(r.n(),0);assert.equal(r.n(),300);const rows=Array.from({length:100},()=>[{ch:'x',zero:'',flags:0}]);const row=C.concat(T.v(1),T.text('x'),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0));socket.frame(0x26,T.v(req),T.v(100),T.v(2),T.v(0),T.v(100),...Array(100).fill(row));handle.loadHistory();r=T.reader(socket.sent.at(-1).slice(5));r.n();assert.equal(r.n(),1);assert.equal(r.n(),1);let historyReq;for(let page=0;page<9;page++){r=T.reader(socket.sent.at(-1).slice(5));historyReq=r.n();socket.frame(0x26,T.v(historyReq),T.v(10000),T.v(2),T.v(page*100),T.v(100),...Array(100).fill(row));handle.loadHistory();}assert.equal(find(root,'terminal-history').textContent.split('\n').length,1001);assert.equal(find(root,'load-history').disabled,true);const before=socket.sent.length;handle.loadHistory();assert.equal(socket.sent.length,before);console.log('PASS 300-row requests, hot-to-cold history and 1000-row cap');
 ({root,handle,socket}=await mount(46));handle.loadHistory();r=T.reader(socket.sent.at(-1).slice(5));const wreq=r.n();const wc=(ch,zero,flags,fg=T.v(0),bg=T.v(0),ul=T.v(0))=>C.concat(T.text(ch),T.v(zero.length),...zero.map(z=>T.text(z)),fg,bg,ul,T.v(flags));
 socket.frame(0x26,T.v(wreq),T.v(1),T.v(1),T.v(0),T.v(1),C.concat(T.v(8),wc('a',[],0),wc('ᄒ',['ᅡ','ᆫ'],8),wc(' ',[],16),wc('글',[],8),wc(' ',[],16),wc('b',[],0),wc('!',[],1|2|32,Uint8Array.from([1,2]),Uint8Array.from([2,10,20,30]),Uint8Array.from([1,1])),wc('?',[],4|128,Uint8Array.from([1,4]),T.v(0))));
 const hist=find(root,'terminal-history'),spans=hist.children[0].children.filter(c=>typeof c==='object'),[span,styledSpan,invSpan]=spans;assert.equal(hist.textContent,'a한글b!?\n');assert.equal(spans.length,3);assert.equal(span.textContent,'한글');assert.equal(span.style.letterSpacing,'var(--wide-spacing)');assert.equal(styledSpan.textContent,'!');assert.equal(styledSpan.style.color,'#9ece6a');assert.equal(styledSpan.style.backgroundColor,'rgb(10,20,30)');assert.equal(styledSpan.style.fontWeight,'500');assert.equal(styledSpan.style.fontStyle,'italic');assert.equal(styledSpan.style.textDecoration,'underline');assert.equal(styledSpan.style.textDecorationColor,'#f7768e');assert.equal(invSpan.textContent,'?');assert.equal(invSpan.style.color,'#1a1b26');assert.equal(invSpan.style.backgroundColor,'#7aa2f7');assert.equal(invSpan.style.opacity,'.65');assert.match(handle.text(),/^a한글b!\?/);handle.unmount();console.log('PASS history <pre>: NFD joined to NFC, wide runs spaced to two cells, ANSI colors and styles preserved');
 handle.unmount();let lastStatus;({root,handle,socket}=await mount(99,{onStatus:s=>{lastStatus=s;}}));const uncertain=handle.sendKey('ctrlC');socket.close();await assert.rejects(uncertain,/Input delivery is uncertain; input will not be resent\./);assert.match(lastStatus.message,/will not be resent/);assert.equal(inputs(socket).length,1);handle.unmount();console.log('PASS disconnect uncertainty, no retry and listener cleanup');
 ({root,handle,socket}=await mount());const canvas=find(root,'terminal-grid'),calls=canvas.calls;await advance(16);
 const line=(n,text)=>C.concat(T.v(2),T.v(n),T.v([...text].length),...[...text].map(ch=>C.concat(T.text(ch),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0)))),cursor=(line,column)=>C.concat(T.v(3),T.v(line),T.v(column),T.v(1));
 const update=(...ops)=>socket.frame(0x21,T.v(2),T.v(2),T.v(1),T.v(ops.length),...ops),drawn=async()=>{const before={...calls};await advance(16);return {text:calls.fillText-before.fillText,font:calls.font-before.font,draws:calls.draws-before.draws};};
 update(C.concat(T.v(0),T.v(80),T.v(24)),line(0,'abc'),line(1,'def'),line(2,'ghi'),cursor(1,2));calls.texts=[];let rectBefore=calls.fillRect;assert.deepEqual(await drawn(),{text:3,font:0,draws:1});assert.deepEqual(calls.texts,['abc','def','ghi']);assert.equal(calls.fillRect-rectBefore,25,'24 dirty row clears + cursor only; default #1a1b26 cell backgrounds skip fillRect');calls.texts=null;
 update(line(1,'xy'));assert.deepEqual(await drawn(),{text:1,font:0,draws:1});
 update(cursor(2,0));assert.equal((await drawn()).text,2);
 // 후행 cursor_explicit 바이트(0x01)와 미래 확장 바이트가 붙은 Update도 해독해야 한다(v0.2.25부터 모바일 터미널이 'Trailing screen data'로 안 뜬 회귀).
 socket.frame(0x21,T.v(2),T.v(2),T.v(1),T.v(1),line(1,'zz'),Uint8Array.of(1,9));assert.deepEqual(await drawn(),{text:1,font:0,draws:1},'update with optional postfix still draws');
 update(line(0,'a'));update(line(2,'g'));assert.deepEqual(await drawn(),{text:2,font:0,draws:1});
 update(C.concat(T.v(0),T.v(80),T.v(24)),line(0,'abc'),line(3,'jk'));assert.equal((await drawn()).text,2);
 handle.setFontScale(1.25);assert.deepEqual(await drawn(),{text:2,font:1,draws:1});assert.equal(canvas.width,Math.round(80*8*16/13));
 const wide=(ch,zero)=>C.concat(T.text(ch),T.v(zero.length),...zero.map(z=>T.text(z)),T.v(0),T.v(0),T.v(0),T.v(8)),spacer=C.concat(T.text(' '),T.v(0),T.v(0),T.v(0),T.v(0),T.v(16));
 update(C.concat(T.v(2),T.v(5),T.v(8),wide('ᄒ',['ᅡ','ᆫ']),spacer,wide('ᄀ',['ᅳ','ᆯ']),spacer,wide('한',[]),spacer,wide('글',[]),spacer));calls.texts=[];await drawn();assert.deepEqual(calls.texts,['한','글','한','글']);calls.texts=null;await advance(500);assert.match(canvas.getAttribute('aria-label'),/\n한글한글\n/);assert.match(handle.text(),/^한글한글$/m);
 await advance(500);assert.match(canvas.getAttribute('aria-label'),/^abc\n\n\njk/);handle.unmount();
 console.log('PASS dirty-row canvas redraw: one changed row only, cursor rows, one frame per batch, Reset/font full');
 ({root,handle,socket}=await mount(42));const port=find(root,'terminal-scroll'),capture2=find(root,'live-capture'),command2=find(root,'command-input');let focused=0,commandFocused=0;capture2.addEventListener('focus',()=>focused++);command2.addEventListener('focus',()=>commandFocused++);
 assert.equal(root.dataset.phase,'ready');
 // 터미널 탭·스크롤은 키보드를 열지 않는다. 키 줄의 키보드 버튼만 연다.
 event(port,'pointerdown',{clientX:100,clientY:100});event(port,'pointermove',{clientX:104,clientY:103});event(port,'click');assert.equal(focused,0);
 event(port,'pointerdown',{clientX:100,clientY:100});event(port,'pointermove',{clientX:100,clientY:130});event(port,'click');assert.equal(focused,0);
 const keyboard=find(root,'show-keyboard');assert.equal(event(keyboard,'pointerdown').defaultPrevented,true);event(keyboard,'click');assert.equal(focused,1);assert.equal(keyboard.getAttribute('aria-pressed'),'true');let blurred=0;capture2.addEventListener('blur',()=>blurred++);event(keyboard,'click');assert.equal(blurred,1);assert.equal(keyboard.getAttribute('aria-pressed'),'false');assert.equal(keyboard.getAttribute('aria-label'),'Show keyboard');
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
  socket.screen();await flush();assert.equal(find(root,'display-mode').getAttribute('aria-label'),'Fit to Phone');socket.frame(0x2c,T.v(0));await flush();
  assert.equal(find(root,'display-mode').getAttribute('aria-label'),'Match Desktop');assert.equal(resizes().length,1);handle.unmount();console.log('PASS auto-fit resize pipelined with attach, no duplicate after first frame');}
 await testHistoryGestures();
 await testHistoryPrefetch();
 await testScrollAnchor();
 await testEmptyHistoryHidden();
 await testFailurePaths();
 await testPasteButton();
 await testTextLayer();
 await testLivePreviewAndHeldIme();
 // 스크롤 관성·휠 경로는 가상 시계를 performance.now에도 물려 결정적으로 검사한다.
 context.performance={now:()=>clock};try{await testTuiScrollRouting();await testShortCanvasScroll();await testScrollPhysics();await testHistoryRefreshMerge();await testInitialAndQueuedHistoryScroll();}finally{delete context.performance;}
 console.log('All terminal adapter regression checks passed.');
}
async function testInitialAndQueuedHistoryScroll(){
 // 1. Hidden mount (e.g. opened in Chat view) preloads 300 rows as soon as setVisible(true) is called.
 const hidden=await mount(2010,{visible:false,preferences:{fitToPhone:false}}),hp=find(hidden.root,'terminal-scroll');hp.clientHeight=400;
 assert.equal(hidden.socket.sent.filter(a=>a[4]===0x12).length,0,'hidden mount waits before preloading history');
 await hidden.handle.setVisible(true);
 assert.equal(hidden.socket.sent.filter(a=>a[4]===0x12).length,1,'becoming visible preloads 300 history rows');
 hidden.handle.unmount();
 // 2. Queued refresh while initial loadHistory is in flight + unscrollable touch drag continuing once history arrives.
 const {root,handle,socket}=await mount(2011,{preferences:{fitToPhone:false}}),port=find(root,'terminal-scroll'),hist=find(root,'terminal-history'),grid=find(root,'terminal-grid'),controls=find(root,'terminal-history-controls');
 let top=0;port.clientHeight=400;
 Object.defineProperty(grid,'offsetTop',{get:()=>(controls.hidden?0:32)+hist.children.length*18});
 Object.defineProperty(port,'scrollHeight',{get:()=>grid.offsetTop+400});
 Object.defineProperty(port,'scrollTop',{get:()=>top,set:v=>{top=Math.max(0,Math.min(v,port.scrollHeight-port.clientHeight));}});
 const requests=()=>socket.sent.filter(a=>a[4]===0x12),row=ch=>C.concat(T.v(1),T.text(ch),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0));
 handle.loadHistory();assert.equal(requests().length,1);
 // User starts pulling down while scrollHeight === clientHeight (400) and initial request is still pending; new output arrives meanwhile.
 socket.screen('newer output');await flush();
 event(port,'touchstart',{touches:[{clientX:150,clientY:100}]});
 event(port,'touchmove',{touches:[{clientX:150,clientY:120}]});
 // Initial history reply arrives with 10 rows -> queued refresh fires immediately, and continuing the same touch drag scrolls up.
 const req1=T.reader(requests().at(-1).slice(5)).n();
 socket.frame(0x26,T.v(req1),T.v(10),T.v(0),T.v(0),T.v(10),...Array(10).fill(row('a')));
 assert.equal(requests().length,2,'refresh requested during pending loadHistory is queued and sent after reply');
 const beforeDragTop=port.scrollTop;
 const moveEvt=event(port,'touchmove',{touches:[{clientX:150,clientY:160}]});
 assert(moveEvt.defaultPrevented,'unscrollable-at-touchstart drag manually scrolls once history arrives');
 assert.equal(port.scrollTop,beforeDragTop-40);
 event(port,'touchend',{touches:[]});
 handle.unmount();
 console.log('PASS hidden session preloads history on setVisible(true), queued refresh fires after in-flight load, unscrollable touchstart scrolls after history arrives');
}
// 대체 화면(TUI)은 셸 기록을 접고 위·아래 쓸기를 모두 TUI에 보고한다. 보고는 입력 ACK·커서·라이브 복귀와 분리하고, 늦은 기록 응답은 TUI 화면을 건드리지 않는다.
async function testShortCanvasScroll(){
 for(const fit of [false,true])for(const flags of [120,64|128])for(const gap of [1,16,304]){
  const {root,handle,socket}=await mount(2100+gap+flags+(fit?1000:0),{preferences:{fitToPhone:false}}),port=find(root,'terminal-scroll');let top=0;
  port.clientHeight=432+gap;port.scrollHeight=port.clientHeight;
  Object.defineProperty(port,'scrollTop',{get:()=>top,set:v=>{top=Math.max(0,Math.min(v,port.scrollHeight-port.clientHeight));}});
  socket.frame(0x2b,Uint8Array.of(0),T.v(flags));
  if(fit){const pending=handle.fitToPhone(true);socket.frame(0x2c,T.v(0));await pending;}
  const drag=(start,end)=>{event(port,'touchstart',{touches:[{clientX:150,clientY:start}]});const e=event(port,'touchmove',{touches:[{clientX:150,clientY:end}]});event(port,'touchend',{touches:[]});assert(e.defaultPrevented,'a short canvas must deliver the gesture to the TUI');};
  const base=inputs(socket).length;drag(100,160);drag(300,240);const sent=inputs(socket).slice(base).join('');
  if(flags===120)assert.match(sent,/^(\x1b\[<64;\d+;\d+M){4}(\x1b\[<65;\d+;\d+M){4}$/);
  else assert.equal(sent,'\x1b[A'.repeat(4)+'\x1b[B'.repeat(4));
  const wheelBase=inputs(socket).length;assert(event(port,'wheel',{deltaX:0,deltaY:-30,deltaMode:0}).defaultPrevented);
  if(flags===120)assert.match(inputs(socket).slice(wheelBase).join(''),/^(\x1b\[<64;\d+;\d+M){2}$/);
  else assert.equal(inputs(socket).slice(wheelBase).join(''),'\x1b[A'.repeat(2));
  assert.equal(top,0,'DOM cannot scroll into the gap below the canvas');handle.unmount();
 }
 console.log('PASS short canvases route both scroll directions to SGR/DEC 1007, fitted and original size, with clamped DOM scroll positions');
}
async function testTuiScrollRouting(){
 const {root,handle,socket}=await mount(2001),port=find(root,'terminal-scroll'),hist=find(root,'terminal-history'),grid=find(root,'terminal-grid'),controls=find(root,'terminal-history-controls'),latest=find(root,'terminal-latest'),preview=find(root,'live-input-copy').children[1];let top=0;port.clientHeight=432;
 Object.defineProperty(grid,'offsetTop',{get:()=>hist.hidden?0:(controls.hidden?0:32)+hist.children.length*18});Object.defineProperty(port,'scrollHeight',{get:()=>grid.offsetTop+(grid.hidden?0:432)});Object.defineProperty(port,'scrollTop',{get:()=>top,set:v=>{top=Math.max(0,Math.min(v,port.scrollHeight-port.clientHeight));}});
 const requests=()=>socket.sent.filter(a=>a[4]===0x12),row=C.concat(T.v(1),T.text('x'),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0)),reply=(total,pages,n)=>{const r=T.reader(requests().at(-1).slice(5));socket.frame(0x26,T.v(r.n()),T.v(total),T.v(pages),T.v(0),T.v(n),...Array(n).fill(row));};
 const touch=(type,x,y)=>event(port,type,{touches:x==null?[]:[{clientX:x,clientY:y}]}),drag=(from,to)=>{touch('touchstart',150,from);touch('touchmove',150,from+(to>from?10:-10));const e=touch('touchmove',150,to);touch('touchend');return e;},modes=f=>socket.frame(0x2b,Uint8Array.of(0),T.v(f)),since=n=>inputs(socket).slice(n).join('');
 handle.loadHistory();reply(5000,0,100);assert.equal(hist.children.length,100);assert.equal(top,1832);socket.screen('before alt');await flush();handle.loadHistory(false,true);
 modes(4|16|64|128);assert.equal(root.dataset.alt,'true');assert.equal(hist.hidden,true);assert.equal(controls.hidden,true);assert.equal(grid.hidden,false);assert.equal(top,0);assert.equal(latest.hidden,true);
 reply(120,0,120);assert.equal(hist.children.length,100,'a reply requested before ALT does not touch the TUI view');const n=requests().length;handle.loadHistory();assert.equal(requests().length,n,'automatic history loads stop in ALT');
 let base=inputs(socket).length,e=drag(100,160);assert(e.defaultPrevented);assert.match(since(base),/^(\x1b\[<64;\d+;\d+M){4}$/,'finger down scrolls the TUI up (first 10px counted)');
 base=inputs(socket).length;drag(300,240);assert.match(since(base),/^(\x1b\[<65;\d+;\d+M){4}$/);assert.equal(requests().length,n);assert.equal(latest.hidden,true);assert.equal(hist.children.length,100);
 await advance(10000);assert.equal(root.dataset.phase,'ready','scroll reports never wait for input confirmation');assert.equal(socket.readyState,1);assert.equal(preview.dataset.preview,'false');assert.equal(top,0);socket.autoAck=true;await handle.sendKey('enter');socket.autoAck=false;
 base=inputs(socket).length;e=event(port,'wheel',{deltaX:0,deltaY:-30,deltaMode:0});assert(e.defaultPrevented);assert.match(since(base),/^(\x1b\[<64;\d+;\d+M){2}$/);base=inputs(socket).length;event(port,'wheel',{deltaX:0,deltaY:1,deltaMode:1});assert.match(since(base),/^\x1b\[<65;\d+;\d+M$/,'reversing the wheel answers at once');base=inputs(socket).length;event(port,'wheel',{deltaX:0,deltaY:-1,deltaMode:2});assert.equal((since(base).match(/\x1b\[<64;/g)||[]).length,30);
 // Codex TUI mode: SGR mouse enabled, alternate screen enabled, DEC 1007 disabled (4|16|64 or 30|64).
 modes(30|64);base=inputs(socket).length;drag(100,160);drag(300,240);assert.match(since(base),/^(\x1b\[<64;\d+;\d+M){4}(\x1b\[<65;\d+;\d+M){4}$/,'Codex ALT_SCREEN + SGR mouse without DEC 1007 scrolls in both directions');
 modes(64|128);base=inputs(socket).length;drag(100,160);drag(300,240);assert.equal(since(base),'\x1b[A'.repeat(4)+'\x1b[B'.repeat(4),'DEC 1007 sends arrows');
 modes(1|64|128);base=inputs(socket).length;drag(100,130);assert.equal(since(base),'\x1bOA\x1bOA','application cursor arrows');
 modes(4|64|128);base=inputs(socket).length;drag(100,160);assert.equal(since(base),'','mouse without SGR invents no input');assert.equal(requests().length,n);
 modes(4|16|64|128);handle.toggleShift(true);base=inputs(socket).length;e=drag(100,160);assert.equal(since(base),'','sticky Shift bypasses the TUI');assert.equal(find(root,'shift-toggle').getAttribute('aria-pressed'),'false');drag(100,130);assert.match(since(base),/^(\x1b\[<64;\d+;\d+M){2}$/);
 // When ALT+Fit has a shrunk viewport (e.g. keyboard open) so canvas overflows (hi > lo), local canvas scroll works before reporting to TUI.
 const fitP=handle.fitToPhone(true);socket.frame(0x2c,T.v(0));await fitP;
 port.clientHeight=200;top=100;base=inputs(socket).length;drag(100,140);assert.equal(top,60,'ALT+Fit overflowing canvas scrolls locally first');assert.equal(since(base),'');
 const offP=handle.fitToPhone(false);socket.frame(0x2c,T.v(0));await offP;
 port.clientHeight=432;top=0;
 modes(4|16);assert.equal(root.dataset.alt,'false');assert.equal(hist.hidden,false);assert.equal(controls.hidden,false);assert.equal(top,1832,'leaving ALT follows live output again');
 base=inputs(socket).length;drag(100,160);assert.match(since(base),/^(\x1b\[<64;\d+;\d+M){4}$/,'inline mouse TUI owns a gesture that starts on live output');assert.equal(requests().length,n);
 // At the bottom, a short live canvas can share the viewport with history.
 // Ownership follows the finger/wheel position, not just the viewport offset.
 grid.getBoundingClientRect=()=>({top:250,left:0});base=inputs(socket).length;
 e=drag(100,160);assert.equal(e.defaultPrevented,false);assert.equal(since(base),'','visible history keeps a native drag even at the live scroll offset');
 await advance(201);e=event(port,'wheel',{deltaX:0,deltaY:-30,clientX:150,clientY:100});assert.equal(e.defaultPrevented,false);assert.equal(since(base),'','wheel over visible history stays local');
 await advance(201);e=event(port,'wheel',{deltaX:0,deltaY:-30,clientX:150,clientY:300});assert(e.defaultPrevented);assert.match(since(base),/^(\x1b\[<64;\d+;\d+M){2}$/);
 delete grid.getBoundingClientRect;
 port.scrollTop=500;event(port,'scroll');base=inputs(socket).length;e=drag(100,160);assert.equal(e.defaultPrevented,false);assert.equal(since(base),'','a gesture started in shell history stays native');
 modes(4|16|64|128);const m=requests().length;handle.loadHistory(true);assert.equal(requests().length,m+1,'explicit reveal is still allowed in ALT');reply(100,0,100);assert.equal(hist.hidden,false);handle.unmount();assert.equal(root.dataset.alt,undefined);
 console.log('PASS ALT folds shell history, both directions reach the TUI (SGR, Codex 30|64, 1007 arrows, SS3), report path skips ACK/cursor, wheel deltaMode, Shift bypass, late replies ignored, inline live gestures');
}
// 손가락 스크롤 물리: 첫 이동 반영, 방향 반전 즉시 반응, 지수 감속 관성과 중단 조건, 넘침 있을 때만 가로 잠금, 터치 중 글자 층 보류.
async function testShortCanvasScroll(){
 // Fit rounds rows down, so even a fraction of a cell leaves the canvas shorter
 // than its viewport. Real scrollTop cannot enter the negative logical range.
 for(const fit of [false,true])for(const flags of [20,84,192,193])for(const extra of [1,8,320]){
  const {root,handle,socket}=await mount(2100+flags+extra,{preferences:{fitToPhone:false}}),port=find(root,'terminal-scroll');let top=0;
  port.clientHeight=432+extra;port.scrollHeight=port.clientHeight;Object.defineProperty(port,'scrollTop',{get:()=>top,set:v=>{top=Math.max(0,Math.min(v,port.scrollHeight-port.clientHeight));}});
  if(fit){const pending=handle.fitToPhone(true);socket.frame(0x2c,T.v(0));await pending;}
  socket.frame(0x2b,Uint8Array.of(0),T.v(flags));const n=inputs(socket).length;
  const touch=(type,y)=>event(port,type,{touches:y==null?[]:[{clientX:150,clientY:y}]});
  touch('touchstart',100);touch('touchmove',130);touch('touchend');touch('touchstart',150);touch('touchmove',120);touch('touchend');
  const bytes=inputs(socket).slice(n).join('');
  if(flags&16)assert.match(bytes,/^(\x1b\[<64;\d+;\d+M){2}(\x1b\[<65;\d+;\d+M){2}$/);
  else assert.equal(bytes,(flags&1?'\x1bOA':'\x1b[A').repeat(2)+(flags&1?'\x1bOB':'\x1b[B').repeat(2));
  assert.equal(top,0,'unscrollable canvas reports remotely without inventing negative scrollTop');handle.unmount();
 }
 console.log('PASS short canvas (1/8/320px gap), Fit on/off, inline/ALT SGR and normal/application 1007 arrows scroll both directions');
 // A TUI gesture scrolls the visible canvas locally before reporting only the
 // remaining distance. Its first tiny move must already reserve the gesture.
 for(const fit of [false,true]){
  const {root,handle,socket}=await mount(2600+Number(fit),{preferences:{fitToPhone:false}}),port=find(root,'terminal-scroll');let top=232;
  port.clientHeight=200;port.scrollHeight=432;Object.defineProperty(port,'scrollTop',{get:()=>top,set:v=>{top=Math.max(0,Math.min(v,232));}});
  if(fit){const pending=handle.fitToPhone(true);socket.frame(0x2c,T.v(0));await pending;}
  socket.frame(0x2b,Uint8Array.of(0),T.v(84));top=232;const n=inputs(socket).length;
  const touch=(type,y)=>event(port,type,{touches:y==null?[]:[{clientX:150,clientY:y}]});
  touch('touchstart',100);assert(touch('touchmove',102).defaultPrevented,'reserve TUI before the 6px axis threshold');assert(touch('touchmove',110).defaultPrevented);assert.equal(top,222);assert.equal(inputs(socket).length,n);
  assert(touch('touchmove',420).defaultPrevented);assert.equal(top,0);assert.match(inputs(socket).slice(n).join(''),/^(\x1b\[<64;\d+;\d+M){6}$/,'232px local + 88px remote keeps the boundary remainder');
  touch('touchmove',400);assert.equal(top,20,'reversing immediately scrolls the canvas locally');touch('touchcancel');const before=inputs(socket).length;await advance(200);assert.equal(inputs(socket).length,before,'cancelled gestures never fling');handle.unmount();
 }
 console.log('PASS routed touch reserves first tiny move, scrolls canvas then reports boundary remainder, reverses locally and cancels without momentum');
}
async function testScrollPhysics(){
 const {root,handle,socket}=await mount(2002),port=find(root,'terminal-scroll'),layer=find(root,'terminal-text');port.clientHeight=432;port.scrollLeft=0;await advance(16);
 const touch=(type,x,y)=>event(port,type,{touches:x==null?[]:[{clientX:x,clientY:y}]}),up=()=>(inputs(socket).join('').match(/\x1b\[<64;/g)||[]).length,down=()=>(inputs(socket).join('').match(/\x1b\[<65;/g)||[]).length;
 socket.frame(0x2b,Uint8Array.of(0),T.v(4|16|64|128));
 touch('touchstart',100,100);touch('touchmove',100,130);assert.equal(up(),2,'the move that picks the axis keeps its distance');touch('touchmove',100,150);assert.equal(up(),3);touch('touchmove',100,135);assert.equal(down(),1,'15px back reverses at once (no carried remainder)');touch('touchend');await advance(100);assert.equal(up(),3);assert.equal(down(),1);
 const fling=async()=>{touch('touchstart',100,100);await advance(16);touch('touchmove',100,130);await advance(16);touch('touchmove',100,160);touch('touchend');};
 await fling();let count=up(),frames=[];for(let i=0;i<20;i++){await advance(16);frames.push(up()-count);count=up();}
 assert(frames[0]>0&&frames.every(f=>f<=8),'momentum continues after release, at most 8 detents a frame');assert(frames.slice(0,5).reduce((a,b)=>a+b)>frames.slice(15).reduce((a,b)=>a+b),'momentum decays');
 await advance(2000);count=up();await advance(500);assert.equal(up(),count,'momentum stops');
 await fling();await advance(32);touch('touchstart',100,100);count=up();await advance(500);assert.equal(up(),count,'a new touch stops momentum');touch('touchend');
 touch('touchstart',100,100);await advance(16);touch('touchmove',100,160);await advance(16);touch('touchmove',100,145);const revUp=up(),revDown=down();touch('touchend');await advance(32);assert.equal(up(),revUp,'reversal before release never flings in the old direction');assert(down()>=revDown);await advance(2000);
 await fling();await advance(32);socket.frame(0x2b,Uint8Array.of(0),T.v(4|16));count=up();await advance(500);assert.equal(up(),count,'a mode change stops momentum');
 await fling();const hide=handle.setVisible(false);socket.frame(0x2c,T.v(0));await hide;count=up();await advance(500);assert.equal(up(),count,'hiding stops momentum');await handle.setVisible(true);
 socket.frame(0x2b,Uint8Array.of(0),T.v(0));port.scrollWidth=port.clientWidth;let e;touch('touchstart',200,300);e=touch('touchmove',188,292);touch('touchmove',180,200);assert.equal(e.defaultPrevented,false);assert.equal(port.scrollLeft,0,'no horizontal overflow keeps a diagonal swipe vertical');touch('touchend');
 port.scrollWidth=1000;touch('touchstart',200,300);touch('touchmove',190,292);touch('touchmove',150,280);assert.equal(port.scrollLeft,0,'10px against 8px is still vertical');touch('touchend');
 touch('touchstart',300,200);await advance(16);e=touch('touchmove',270,201);assert(e.defaultPrevented);await advance(16);touch('touchmove',240,202);assert.equal(port.scrollLeft,60);touch('touchend');await advance(16);assert(port.scrollLeft>60,'horizontal pan glides after release');await advance(2000);const left=port.scrollLeft;await advance(500);assert.equal(port.scrollLeft,left);
 touch('touchstart',100,100);socket.screen('during touch');await advance(16);assert.doesNotMatch(layer.children[0].textContent,/during touch/,'text rows wait while a finger is down');touch('touchend');assert.match(layer.children[0].textContent,/^during touch/);handle.unmount();
 console.log('PASS touch keeps its first distance, reverses without a deadzone, exponential momentum stops on touch/mode/hide, axis lock needs overflow, text layer waits for the finger');
}
// 새로고침이 같은 기록을 돌려주면 DOM을 건드리지 않고, 새 행만 덧붙이며, 1000행을 넘으면 맨 앞을 잘라 낸다. 창 모드에서 바닥까지 내리면 라이브로 돌아간다.
async function testHistoryRefreshMerge(){
 const setup=async id=>{const {root,handle,socket}=await mount(id),port=find(root,'terminal-scroll'),hist=find(root,'terminal-history');port.clientHeight=400;
  const requests=()=>socket.sent.filter(a=>a[4]===0x12),last=()=>{const r=T.reader(requests().at(-1).slice(5));return {id:r.n(),source:r.n(),skip:r.n(),count:r.n()};};
  const row=t=>C.concat(T.v([...t].length),...[...t].map(ch=>C.concat(T.text(ch),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0))));
  return {root,handle,socket,port,hist,requests,last,reply:(total,pages,skip,texts)=>socket.frame(0x26,T.v(last().id),T.v(total),T.v(pages),T.v(skip),T.v(texts.length),...texts.map(row))};};
 const names=(from,n,p='r')=>Array.from({length:n},(_,i)=>p+(from+i)),same=(nodes,hist,offset=0)=>nodes.every((c,i)=>hist.children[i+offset]===c);
 let t=await setup(2003);t.handle.loadHistory();t.reply(100,0,0,names(0,100));let nodes=[...t.hist.children];
 t.handle.loadHistory(false,true);assert.equal(t.last().count,300);t.reply(100,0,0,names(0,100));assert.equal(t.hist.children.length,100);assert(same(nodes,t.hist),'an identical refresh keeps every row node');
 t.handle.loadHistory(false,true);t.reply(105,0,0,[...names(0,100),...names(0,5,'n')]);assert.equal(t.hist.children.length,105);assert(same(nodes,t.hist),'new output only appends');assert.equal(t.hist.children[104].textContent,'n4\n');t.handle.unmount();
 t=await setup(2007);t.handle.loadHistory();t.reply(100,0,0,Array(100).fill('same'));nodes=[...t.hist.children];t.handle.loadHistory(false,true);t.reply(105,0,0,Array(105).fill('same'));assert.equal(t.hist.children.length,105);assert(same(nodes,t.hist),'repeated-row refresh still appends at the tail');t.handle.unmount();
 t=await setup(2004);t.handle.loadHistory();t.reply(1000,0,0,names(0,1000));nodes=[...t.hist.children];t.handle.loadHistory(false,true);assert.equal(t.last().count,1000);t.reply(1005,0,0,[...names(5,995),...names(0,5,'n')]);
 assert.equal(t.hist.children.length,1000);assert(same(nodes.slice(5),t.hist),'the bound drops only the oldest rows');assert.equal(t.hist.children[999].textContent,'n4\n');assert.equal(find(t.root,'terminal-grid').hidden,false);t.handle.unmount();
 t=await setup(2005);t.handle.loadHistory();t.reply(5000,0,0,names(0,600));nodes=[...t.hist.children];t.handle.loadHistory(false,true);assert.equal(t.last().count,600);t.reply(5005,0,0,[...names(5,595),...names(0,5,'n')]);
 assert.equal(t.hist.children.length,605);assert(same(nodes,t.hist));t.handle.loadHistory();assert.deepEqual([t.last().source,t.last().skip],[0,605],'older paging continues after the kept rows');t.handle.unmount();
 t=await setup(2006);const grid=find(t.root,'terminal-grid');t.port.scrollHeight=20000;t.handle.loadHistory();t.reply(5000,1,0,names(0,600));t.handle.loadHistory();t.reply(5000,1,600,names(600,600));assert.equal(grid.hidden,true);
 t.port.scrollTop=19000;event(t.port,'scroll');assert.equal(grid.hidden,true);t.port.scrollTop=19590;event(t.port,'scroll');assert.equal(grid.hidden,false,'scrolling to the bottom of a history window returns to live');assert.deepEqual([t.last().source,t.last().skip],[0,0]);assert.equal(find(t.root,'terminal-latest').hidden,true);t.handle.unmount();
 console.log('PASS history refresh keeps identical rows, appends new output, trims the oldest past 1000, keeps paging position, window bottom returns to live');
}
async function testLivePreviewAndHeldIme(){
 // P0-1: Reserved input preview keeps terminal geometry and text through screen/ACK changes.
 let statuses=[];const {root,handle,socket}=await mount(1960,{preferences:{inputMode:'direct'},onStatus:s=>statuses.push(s)});
 const dock=find(root,'command-dock'),preview=find(root,'live-input-copy').children[1],capture=find(root,'live-capture');
 assert.equal(dock.children[0],find(root,'terminal-question'));assert.equal(dock.children[0].hidden,true);assert.equal(dock.children[1],find(root,'accessory-row'));assert.equal(dock.children.length,3);
 assert.equal(preview.getAttribute('aria-live'),'polite');assert.equal(preview.dataset.preview,'false');
 handle.focus();capture.value='hello';event(capture,'input');
 assert.equal(preview.dataset.preview,'true');assert.equal(preview.textContent,'hello');
 socket.screen('echo hello');await flush();assert.equal(preview.dataset.preview,'true');assert.equal(preview.textContent,'hello');assert.equal(dock.children.length,3);
 const longText='0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJ';
 capture.value='hello'+longText;event(capture,'input');
 assert.equal(preview.dataset.preview,'true');assert.equal(preview.textContent,'hello'+longText);
 const r1=T.reader(socket.sent.at(-2).slice(5)),id1=r1.n(),r2=T.reader(socket.sent.at(-1).slice(5)),id2=r2.n();
 socket.frame(0x27,T.v(id1),T.v(0));assert.equal(preview.dataset.preview,'true');
 socket.frame(0x27,T.v(id2),T.v(0));assert.equal(preview.dataset.preview,'true');assert.equal(preview.textContent,'hello'+longText);
 // P1-1: Repeated ready status with identical phase/message skips DOM write and onStatus callback.
 const statusCount=statuses.length;socket.screen('same size');await flush();assert.equal(statuses.length,statusCount);
 // Native IME owns its entire preedit, including pauses and jamo Backspace.
 socket.autoAck=true;handle.sendKey('escape');await flush();const base=inputs(socket).length,preedit=find(root,'terminal-preedit');
 event(capture,'compositionstart');
 for(const text of ['ㅎ','하','한','한ㄱ','한글']){capture.value=text;event(capture,'input',{isComposing:true,inputType:'insertCompositionText'});assert.equal(preview.textContent,text);assert.equal(preedit.hidden,false);assert.equal(preedit.textContent,text);}
 await advance(1500);assert.deepEqual(inputs(socket).slice(base),[],'a pause never commits native IME');
 assert.equal(event(capture,'beforeinput',{inputType:'deleteContentBackward',isComposing:true}).defaultPrevented,false);
 assert.equal(event(capture,'keydown',{key:'Backspace',isComposing:true}).defaultPrevented,false);
 assert.equal(capture.value,'한글','the browser owns deletion');
 for(const text of ['한그','한ㄱ','한']){capture.value=text;event(capture,'input',{isComposing:true});assert.equal(preedit.textContent,text);}
 event(capture,'compositionend',{data:'unreliable'});await advance(0);assert.deepEqual(inputs(socket).slice(base),['한']);assert.equal(preedit.hidden,true);
 // The Enter accepting IME must not insert a newline or submit a command.
 await handle.sendKey('escape');handle.focus();const accepting=inputs(socket).length;event(capture,'compositionstart');capture.value='한';event(capture,'input',{isComposing:true});event(capture,'compositionend',{data:'한'});assert.equal(api.updateBusy('1960'),true);assert.equal(event(capture,'keydown',{key:'Enter',keyCode:13}).defaultPrevented,true);assert.equal(event(capture,'beforeinput',{inputType:'insertLineBreak'}).defaultPrevented,true);await advance(0);assert.deepEqual(inputs(socket).slice(accepting),['한']);assert.equal(find(root,'paste-review'),undefined);
 // Back-to-back commits cannot cancel the previous zero-delay finalization.
 event(capture,'compositionstart');capture.value='한가';event(capture,'input',{isComposing:true});event(capture,'compositionend',{data:'가'});
 event(capture,'compositionstart');capture.value='한가나';event(capture,'input',{isComposing:true});event(capture,'compositionend',{data:'나'});await advance(0);
 assert.deepEqual(inputs(socket).slice(accepting),['한','가','나']);
 // A late DOM edit after compositionend wins over stale event.data.
 event(capture,'compositionstart');capture.value='한가나ㅎ';event(capture,'input',{isComposing:true});event(capture,'compositionend',{data:'ㅎ'});capture.value='한가나한';event(capture,'input',{inputType:'insertFromComposition'});await advance(0);
 assert.deepEqual(inputs(socket).slice(-1),['한']);
 // Toolbar controls ask the native IME to finish, then send text before Enter.
 event(capture,'compositionstart');capture.value='한가나한마';event(capture,'input',{isComposing:true});const enter=handle.sendKey('enter');
 assert.equal(inputs(socket).at(-1),'한');event(capture,'compositionend',{data:'마'});await advance(0);await enter;
 assert.deepEqual(inputs(socket).slice(-2),['마','\r']);assert.equal(preedit.hidden,true);
 // Native final input arriving after that explicit control is never replayed.
 capture.value='한가나한마';event(capture,'input',{inputType:'insertFromComposition'});await flush();assert.equal(inputs(socket).at(-1),'\r');assert.equal(capture.value,'');
 handle.focus();const fallback=inputs(socket).length;
 for(const value of ['ㅎ','하','한']){capture.value=value;event(capture,'input',{inputType:'insertText'});}
 await advance(1500);assert.equal(inputs(socket).length,fallback);assert.equal(preedit.textContent,'한');
 // Resyllabification and compound final consonants only send stable prefixes.
 for(const value of ['하나','하나 ','하나 값','하나 갑사']){capture.value=value;event(capture,'input',{inputType:'insertText'});}
 assert.deepEqual(inputs(socket).slice(fallback),['하','나 ','갑']);assert.equal(preedit.textContent,'사');
 await handle.sendKey('enter');assert.deepEqual(inputs(socket).slice(-2),['사','\r']);
 // A ten-key vowel dot can resegment the preceding final consonant too.
 handle.focus();const dots=inputs(socket).length;for(const value of ['감','감ㆍ','가머','가머 ']){capture.value=value;event(capture,'input');}assert.deepEqual(inputs(socket).slice(dots),['가','머 ']);await handle.sendKey('enter');
 // Decomposed jamo are held together, never split by Unicode codepoint.
 handle.focus();const decomposed=inputs(socket).length;capture.value='한';event(capture,'input');await advance(1500);assert.equal(inputs(socket).length,decomposed);assert.equal(preedit.textContent,'한');
 capture.value+=' ';event(capture,'input');await flush();assert.equal(inputs(socket).at(-1),'한 ');
 // Cancellation and switching panes discard local preedit without sending it.
 await handle.sendKey('escape');handle.focus();const cancelled=inputs(socket).length;event(capture,'compositionstart');capture.value='취소';event(capture,'input',{isComposing:true});capture.value='';event(capture,'compositionend',{data:''});await advance(0);assert.equal(inputs(socket).length,cancelled);assert.equal(preedit.hidden,true);
 // Quick insertion/paste waits for native preedit instead of clearing it.
 await handle.sendKey('escape');handle.focus();const quick=inputs(socket).length;event(capture,'compositionstart');capture.value='한';event(capture,'input',{isComposing:true});assert.equal(handle.insertText('/help'),true);assert.equal(capture.value,'한');assert.equal(inputs(socket).length,quick);event(capture,'compositionend',{data:'한'});await advance(0);assert.deepEqual(inputs(socket).slice(quick),['한','/help']);capture.value='한';event(capture,'input',{inputType:'insertFromComposition'});assert.deepEqual(inputs(socket).slice(quick),['한','/help']);handle.focus();
 // Native final events may fire synchronously from blur while switching panes.
 await handle.sendKey('escape');handle.focus();event(capture,'compositionstart');capture.value='한';event(capture,'input',{isComposing:true});const blur=capture.blur.bind(capture);capture.blur=()=>{blur();event(capture,'compositionend',{data:'한'});event(capture,'input',{inputType:'insertFromComposition'});};const switching=inputs(socket).length;handle.setInputActive(false);assert.equal(find(root,'command-input').value,'한');assert.equal(inputs(socket).length,switching);assert.doesNotMatch(find(root,'terminal-status').textContent,/Resume terminal input/);capture.blur=blur;handle.setInputActive(true);handle.setInputMode('direct');handle.focus();find(root,'command-input').value='';event(find(root,'command-input'),'input');
 // End-pending quick insertion must not synchronously refocus and replay a late commit.
 await handle.sendKey('escape');handle.focus();const endingQuick=inputs(socket).length;event(capture,'compositionstart');capture.value='한';event(capture,'input',{isComposing:true});event(capture,'compositionend',{data:'한'});handle.insertText('/help');capture.value='한';event(capture,'input',{inputType:'insertFromComposition'});await handle.sendKey('enter');assert.deepEqual(inputs(socket).slice(endingQuick),['한','/help','\r']);
 // An explicit key during end-finalization also blurs before disarming.
 await handle.sendKey('escape');handle.focus();event(capture,'compositionstart');capture.value='한';event(capture,'input',{isComposing:true});event(capture,'compositionend',{data:'한'});let endingBlurred=false;const originalBlur=capture.blur.bind(capture);capture.blur=()=>{endingBlurred=true;originalBlur();};await handle.sendKey('tab');assert.equal(endingBlurred,true);capture.blur=originalBlur;handle.focus();const afterEnding=inputs(socket).length;capture.value='x';event(capture,'input');await flush();assert.deepEqual(inputs(socket).slice(afterEnding),['x']);
 // A missing native end cannot leave composition stuck after focusing again.
 await handle.sendKey('escape');handle.focus();event(capture,'compositionstart');capture.value='한';event(capture,'input',{isComposing:true});const interrupted=handle.sendKey('tab'),rejected=assert.rejects(interrupted,/Input target changed/);handle.focus();await rejected;const resumed=inputs(socket).length;capture.value='x';event(capture,'input');await flush();assert.deepEqual(inputs(socket).slice(resumed),['x']);await handle.sendKey('escape');handle.focus();
 // Switching the active pane preserves eventless preedit as an unsent draft.
 capture.value='남겨';event(capture,'input');handle.setInputActive(false);assert.equal(find(root,'command-input').value,'겨');assert.equal(preedit.hidden,true);handle.setInputActive(true);assert.equal(handle.getInputMode(),'direct','temporary draft recovery never changes the chosen input mode');handle.setInputMode('direct');handle.focus();
 // invalidateInput() clears the reserved preview immediately.
 handle.focus();event(capture,'compositionstart');capture.value='가';event(capture,'input',{isComposing:true});
 assert.equal(preview.dataset.preview,'true');handle.setInputMode('buffered');assert.equal(preview.dataset.preview,'false');assert.equal(preview.textContent,'Tap to show keyboard');
 handle.unmount();
 // P1-1: Direct typing before ready does not permanently block contextBlocked.
 const unreadyRoot=new Element('div'),unreadyHandle=context.mountFixture(unreadyRoot,1961,{preferences:{inputMode:'direct'}});await flush();
 const unreadySocket=context.sockets.at(-1),unreadyCapture=find(unreadyRoot,'live-capture'),unreadyRecover=unreadyRoot.children[0].children.find(e=>e.textContent==='Resume input');
 unreadyHandle.focus();unreadyCapture.value='early';event(unreadyCapture,'input');await flush();
 assert.equal(unreadyCapture.value,'');assert.equal(unreadyRecover.hidden,true);assert.match(find(unreadyRoot,'terminal-status').textContent,/Wait for the live terminal/);
 unreadySocket.handshake();unreadySocket.screen();unreadySocket.autoAck=true;await flush();
 unreadyHandle.focus();unreadyCapture.value='ready-now';event(unreadyCapture,'input');await flush();
 assert.deepEqual(inputs(unreadySocket),['ready-now']);unreadyHandle.unmount();
 // P1-2 & P1-5: Canvas style-split run batching + non-default bg fillRect, and 2-finger pinch zoom midpoint scroll preservation.
 const {root:r3,handle:h3,socket:s3}=await mount(1962),c3=find(r3,'terminal-grid'),p3=find(r3,'terminal-scroll');await advance(16);
 const cell=(ch,fg=0,bg=0,flags=0)=>C.concat(T.text(ch),T.v(0),fg?C.concat(T.v(1),Uint8Array.of(fg)):T.v(0),bg?C.concat(T.v(1),Uint8Array.of(bg)):T.v(0),T.v(0),T.v(flags));
 c3.calls.texts=[];const rects0=c3.calls.fillRect;
 s3.frame(0x21,T.v(2),T.v(2),T.v(1),T.v(1),C.concat(T.v(2),T.v(0),T.v(5),cell('a'),cell('b'),cell('c',1),cell('d',1),cell('e',0,2)));
 await advance(16);assert.deepEqual(c3.calls.texts,['ab','cd','e']);assert.equal(c3.calls.fillRect-rects0,2,'1 dirty row clear + 1 non-default bg cell fillRect');c3.calls.texts=null;
 p3.scrollLeft=100;p3.scrollTop=200;
 event(p3,'touchstart',{touches:[{clientX:100,clientY:150},{clientX:200,clientY:250}]});
 event(p3,'touchmove',{touches:[{clientX:50,clientY:100},{clientX:250,clientY:300}]});
 event(p3,'touchend',{touches:[]});
 assert.equal(p3.scrollLeft,350);assert.equal(p3.scrollTop,600);h3.unmount();
 console.log('PASS native/eventless Hangul: pause, jamo deletion, boundaries, resyllabification, final DOM, controls/no replay, cancellation, preedit; unready contextBlocked guard, canvas style runs, pinch midpoint');
}
async function testTextLayer(){
 // 캔버스 위 투명 글자 층: 셀 폭 정렬, 바뀐 행 반영, 선택 중에는 고정했다가 선택이 풀리면 반영.
 const {root,socket,handle}=await mount(1951);await advance(16);const layer=find(root,'terminal-text'),canvas=find(root,'terminal-grid');
 assert.equal(layer.getAttribute('aria-hidden'),'true');assert.equal(layer.children.length,24);assert.match(layer.children[0].textContent,/^Krill · live terminal/);
 assert.equal(layer.style.width,canvas.style.width);assert.equal(layer.style.marginTop,`-${canvas.style.height}`);assert.equal(layer.style.fontSize,find(root,'terminal-history').style.fontSize);
 const row=layer.children[0];socket.screen('first edit');await advance(16);assert.match(layer.children[0].textContent,/^first edit/);assert.notEqual(layer.children[0],row);assert.equal(layer.children.length,24);
 let selected=true;context.getSelection=()=>({isCollapsed:!selected,rangeCount:1,getRangeAt:()=>({intersectsNode:node=>node===layer})});
 socket.screen('while selecting');await advance(16);assert.match(layer.children[0].textContent,/^first edit/,'selected rows stay put');
 selected=false;context.document.dispatchEvent(new Event('selectionchange'));assert.match(layer.children[0].textContent,/^while selecting/,'released selection catches up');
 delete context.getSelection;handle.unmount();console.log('PASS canvas text layer aligns with cells, patches dirty rows, holds while selected');
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
async function testPasteButton(){
 let clip='one\r\ntwo';context.navigator={clipboard:{readText:async()=>{if(clip instanceof Error)throw clip;return clip;}}};
 let {root,handle,socket}=await mount(704,{preferences:{inputMode:'direct'}});socket.autoAck=true;const paste=find(root,'paste-key');assert.equal(paste.parent,find(root,'keybar'),'paste scrolls with the terminal key list');assert.equal(paste.getAttribute('aria-label'),'Paste');assert.equal(event(paste,'pointerdown').defaultPrevented,true);
 socket.frame(0x2b,Uint8Array.of(0),T.v(32));event(paste,'click');await flush();assert.deepEqual(inputs(socket),['\x1b[200~one\ntwo\x1b[201~']);
 socket.frame(0x2b,Uint8Array.of(0),T.v(0));clip='ls -la';event(paste,'click');await flush();assert.equal(inputs(socket).at(-1),'ls -la');
 // 권한 거부·API 없음(비보안 연결)은 빈 확인 칸을 열어 길게 눌러 붙여넣게 한다. 빈 칸 전송은 아무것도 보내지 않는다.
 clip=new Error('NotAllowedError');event(paste,'click');await flush();let review=root.children.at(-1).children.at(-1),editor=review.children[0].children[0];assert.equal(editor.tagName,'textarea');assert.equal(editor.value,'');const sent=inputs(socket).length;event(review,'submit');await flush();assert.equal(inputs(socket).length,sent);assert.notEqual(root.children.at(-1).children.at(-1),review);
 context.navigator={};event(paste,'click');await flush();review=root.children.at(-1).children.at(-1);editor=review.children[0].children[0];editor.value='typed';event(review,'submit');await flush();assert.equal(inputs(socket).at(-1),'typed');
 // 일반 입력은 보내지 않고 입력 칸 커서 자리에 넣는다.
 context.navigator={clipboard:{readText:async()=>'hi'}};handle.setInputMode('buffered');const command=find(root,'command-input');command.value='echo ';command.selectionStart=command.selectionEnd=5;const count=inputs(socket).length;event(paste,'click');await flush();assert.equal(command.value,'echo hi');assert.equal(command.selectionStart,7);assert.equal(inputs(socket).length,count);
 delete context.navigator;handle.unmount();
}
async function testFailurePaths(){
 let {root,handle,socket}=await mount(701,{preferences:{inputMode:'direct'},onPhoto:()=>{}});socket.autoAck=true;handle.focus();let capture=find(root,'live-capture');
 const hello=socket.sent.find(a=>a[4]===2),hr=T.reader(hello.slice(5));hr.n();hr.n();assert(hr.u()&8192n);
 const photo=find(root,'photo-attach'),accessory=find(root,'accessory-row'),inputRow=find(root,'input-row');assert.equal(photo.parent,accessory,'Direct mode shares one toolbar for keyboard, keys and photos');assert.equal(accessory.hidden,undefined);assert.equal(inputRow.hidden,true);assert.equal(find(root,'show-keyboard').parent,accessory);assert.equal(capture.parent,accessory,'Direct capture stays focusable outside the hidden input row');assert.equal(find(root,'show-keyboard').hidden,false);assert.equal(find(root,'buffered-command').hidden,true);let picked=0;find(root,'photo-picker').click=()=>picked++;event(photo,'click');assert.equal(picked,1,'Direct photo action opens its existing image picker without sending input');assert.equal(inputs(socket).length,0);assert.equal(find(root,'photo-picker').accept,'image/*');assert.equal(find(root,'file-picker').accept,'image/*');
 assert.equal(find(root,'photo-attach').parent,find(root,'accessory-row'));assert.equal(!find(root,'accessory-row').hidden,true);handle.setInputMode('buffered');assert.equal(find(root,'photo-attach').parent,find(root,'buffered-command'));assert.equal(find(root,'buffered-command').hidden,false);assert.equal(find(root,'show-keyboard').hidden,false);handle.setInputMode('direct');assert.equal(find(root,'photo-attach').parent,find(root,'accessory-row'));assert.equal(find(root,'show-keyboard').hidden,false);handle.focus();
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
// These layout rules complement the real-browser port geometry probe.
function stableStatusLayout(){const css=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.css'),'utf8');
 for(const selector of ['.terminal-status','.terminal-frame>.ghost.small','#connection-banner'])assert(css.split(selector+'{').slice(1).some(rule=>rule.split('}')[0].includes('position:absolute')),selector+' overlays without taking port space');
 assert(!/\[data-keyboard-open=true\] \.command-dock\{padding-bottom:0/.test(css),'keyboard safe area changes continuously');assert(css.includes('calc(env(safe-area-inset-bottom) - var(--keyboard-inset,0px))'));assert(css.includes('.is-split>.terminal-pane[data-active=false] .command-dock{visibility:hidden;pointer-events:none}'));
 console.log('PASS CLS terminal overlays, continuous safe-area inset and reserved inactive split docks');}
if(process.argv.includes('--harness')){
 const directory=process.env.MOBILE_TERMINAL_HARNESS_DIR||'/tmp/mk-mobile/term';fs.mkdirSync(directory,{recursive:true});fs.writeFileSync(path.join(directory,'fixture.js'),`(${fixture})();\n${source}\n globalThis.handle=mountFixture(document.querySelector('main'));setTimeout(()=>{sockets[0].handshake();sockets[0].screen('~/krill  git: mobile-orca-ui');sockets[0].autoAck=true;},0);`);
 fs.writeFileSync(path.join(directory,'term-harness.html'),'<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover"><link rel="stylesheet" href="/app.css"><main class="terminal-mount"></main><script src="/fixture.js"></script>');
 console.log('Harness written to '+directory);
}else{stableStatusLayout();const watchdog=setTimeout(()=>{console.error('Terminal checks did not settle');process.exitCode=1;},5000);main().catch(e=>{console.error(e);process.exitCode=1;}).finally(()=>clearTimeout(watchdog));}
