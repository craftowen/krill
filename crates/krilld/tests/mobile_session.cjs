// Session screen regressions without a browser or a running app.
'use strict';
const assert=require('node:assert/strict'),{getEventListeners}=require('node:events'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
class Element extends EventTarget {
 constructor(tag,text,cls){super();this.tag=tag;this.textContent=text;this.className=cls;this.children=[];this.dataset={};this.attrs={};this.style={};this.scrollLeft=0;}
 append(...nodes){for(const n of nodes){n.parentElement=this;this.children.push(n);}}
 prepend(...nodes){for(const n of nodes)n.parentElement=this;this.children.unshift(...nodes);}
 replaceChildren(...nodes){this.children=[];this.append(...nodes);}
 setAttribute(k,v){this.attrs[k]=v;}
 getAttribute(k){return this.attrs[k];}
 querySelector(selector){return this.children.flatMap(n=>[n,...n.descendants()]).find(n=>selector==='h1'?n.tag==='h1':selector==='[aria-selected=true]'&&n.attrs['aria-selected']==='true');}
 descendants(){return this.children.flatMap(n=>[n,...n.descendants()]);}
 scrollIntoView(){}
 remove(){if(this.parentElement)this.parentElement.children=this.parentElement.children.filter(n=>n!==this);}
}
const longPressCode=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8').split('\n').find(l=>l.includes('function longPress('));
function fixture(sessions=[{id:7,state:'Working',branch:null}],snapshot){
 const timers=new Map(),mounts=[],buttons=[],sheets=[],calls=[],routes=[],abort=new AbortController();let clock=0,id=0;
 const sandbox={AbortController,abort,document:{createElementNS:(_,tag)=>new Element(tag)},setTimeout:(fn,ms)=>{timers.set(++id,{fn,at:clock+ms});return id;},clearTimeout:id=>timers.delete(id),KrillMobile:{terminal:{mount(root,options){const h={options,focused:0,focus(){h.focused++;},unmount(){options.onStatus({phase:'disconnected',message:'Disconnected'});}};mounts.push(h);options.onStatus({phase:'connecting'});return h;}}}};
 vm.createContext(sandbox);vm.runInContext(longPressCode,sandbox);vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/session.js'),'utf8'),sandbox);
 const group={key:'g',label:'Workspace',sessions},state={groups:new Map([['g',group]]),sessions:new Map(sessions.map(s=>[s.id,s])),connection:{phase:'ready'},preferences:{},drafts:new Map(),snapshot};
 const el=(...args)=>new Element(...args),ctx={el,state,route:{groupKey:'g',sessionId:7},signal:abort.signal,button(label,action,cls){const b=el('button',label,cls);b.action=action;buttons.push(b);return b;},header(){const h=el('header'),copy=el('div');copy.append(el('h1'));h.append(copy);return h;},icon:()=>el('svg'),dot:status=>el('span',undefined,`status-dot ${String(status).toLowerCase()}`),longPress:sandbox.longPress,groupRoute:()=>'',go(r){routes.push(r);},rerender(){},savePreferences(){},error(){},photo(){},transport:{},sheet:(title,entries,_,render)=>{entries.title=title;entries.render=render;sheets.push(entries);},closeSheet(){},rpc:async request=>calls.push(request),refresh:async()=>{}};
 const view=sandbox.KrillMobile.screens.session(el('main'),ctx);
 function advance(ms){clock+=ms;for(const [key,t] of [...timers])if(t.at<=clock){timers.delete(key);t.fn();}}
 return {view,state,ctx,abort,longPress:sandbox.longPress,mounts,timers,advance,buttons,sheets,calls,routes};
}
async function main(){
 let f=fixture();assert.equal(f.mounts.length,1);f.mounts[0].options.onStatus({phase:'ready'});f.mounts[0].options.onStatus({phase:'disconnected'});for(let n=0;n<10;n++)f.view.update();assert.equal(f.timers.size,1);f.advance(999);assert.equal(f.mounts.length,1);f.advance(1);assert.equal(f.mounts.length,2);f.mounts[1].options.onStatus({phase:'disconnected'});f.advance(1999);assert.equal(f.mounts.length,2);f.advance(1);assert.equal(f.mounts.length,3);f.mounts[2].options.onStatus({phase:'disconnected'});f.view.cleanup();f.advance(60000);assert.equal(f.mounts.length,3);f.mounts[0].options.onStatus({phase:'disconnected'});assert.equal(f.timers.size,0);
 f=fixture();f.mounts[0].options.onStatus({phase:'disconnected'});f.state.connection.phase='disconnected';f.view.update();f.advance(60000);assert.equal(f.mounts.length,1);f.state.connection.phase='ready';f.view.update();assert.equal(f.mounts.length,2);f.view.cleanup();
 console.log('PASS terminal-only reconnect backoff, generation/cleanup and main connection cancellation');
 f=fixture();const actions=f.buttons.find(b=>b.textContent==='Terminal actions'),git=f.buttons.find(b=>b.textContent==='Git changes');assert(git.hidden);
 const replace=(state,branch=null)=>{const session={id:7,state,branch};f.state.sessions.set(7,session);f.state.groups.set('g',{key:'g',label:'Latest workspace',sessions:[session]});};
 replace('Waiting','main');actions.action();let entries=f.sheets.at(-1);assert(entries.some(e=>e.label==='Approve (Yes)'));assert(entries.some(e=>e.label==='Deny (No)'));f.view.update();assert.equal(git.hidden,false);
 replace('Working');for(const entry of entries.filter(e=>['Approve (Yes)','Deny (No)'].includes(e.label)))await assert.rejects(entry.action(),/no longer waiting/);assert.equal(f.calls.length,0);actions.action();assert(!f.sheets.at(-1).some(e=>e.label==='Approve (Yes)'));f.view.update();assert(git.hidden);
 replace('Blocked');actions.action();entries=f.sheets.at(-1);await entries.find(e=>e.label==='Approve (Yes)').action();await entries.find(e=>e.label==='Deny (No)').action();assert.deepEqual(f.calls.map(c=>c.value),['y','n']);f.state.sessions.delete(7);await assert.rejects(entries.find(e=>e.label==='Approve (Yes)').action(),/no longer waiting/);f.view.cleanup();
 console.log('PASS latest session approval menus, stale action rejection and branch controls');
 f=fixture(undefined,{capabilities:{session_create:true}});f.ctx.rpc=async r=>{f.calls.push(r);return {session:42};};f.ctx.groupRoute=(_,id)=>`/s/${id}`;const add=f.buttons.find(b=>b.textContent==='Create Tab');await add.action();let sheet=f.sheets.at(-1),body=new Element('div');assert.equal(sheet.title,'New session');assert.equal(sheet.length,0);sheet.render(body);
 assert.deepEqual(body.children.map(b=>[b.textContent,b.className,b.children[0].attrs.class]),[['Claude Code','sheet-action provider','agent-icon claude'],['Codex','sheet-action provider','agent-icon codex'],['Terminal','sheet-action provider','agent-icon']]);
 await body.children[0].action();assert.equal(JSON.stringify(f.calls.at(-1)),'{"action":"create_terminal","cwd_session":7,"agent":"claude"}');assert.equal(f.routes.at(-1),'/s/42');assert.equal(add.disabled,false);
 await add.action();body=new Element('div');f.sheets.at(-1).render(body);await body.children[2].action();assert.equal(JSON.stringify(f.calls.at(-1)),'{"action":"create_terminal","cwd_session":7}');
 f.buttons.find(b=>b.textContent==='Terminal actions').action();assert(!f.sheets.at(-1).some(e=>/^New .* session$/.test(e.label)));f.view.cleanup();
 console.log('PASS + opens provider sheet (old-daemon Claude/Codex fallback + Terminal), creates the picked agent and routes to its tab');
 f=fixture(undefined,{capabilities:{session_create:true},agents:[{id:'claude',name:'Claude Code'},{id:'opencode',name:'OpenCode'},{id:'my-bot',name:'My Bot'}]});f.ctx.rpc=async r=>{f.calls.push(r);return {session:43};};await f.buttons.find(b=>b.textContent==='Create Tab').action();body=new Element('div');f.sheets.at(-1).render(body);
 assert.deepEqual(body.children.map(b=>[b.textContent,b.children[0].attrs.class||b.children[0].className]),[['Claude Code','agent-icon claude'],['OpenCode','agent-icon opencode'],['My Bot','agent-icon custom'],['Terminal','agent-icon']]);
 await body.children[2].action();assert.equal(JSON.stringify(f.calls.at(-1)),'{"action":"create_terminal","cwd_session":7,"agent":"my-bot"}');f.view.cleanup();
 f=fixture(undefined,{capabilities:{session_create:true},agents:[]});await f.buttons.find(b=>b.textContent==='Create Tab').action();body=new Element('div');f.sheets.at(-1).render(body);assert.deepEqual(body.children.map(b=>b.textContent),['Terminal']);f.view.cleanup();
 console.log('PASS + sheet lists snapshot agents (custom profiles included) then Terminal; empty list leaves Terminal only');
 await testTransportAuthRetries();
 console.log('PASS stage-2 transport retry cooldown vs explicit 1008 auth rejection');
 await testPipelinedHandshakeAndPolling();
 console.log('PASS pipelined ClientHello+snapshot before auth reply, hidden-tab skip and live-terminal poll interval');
 f=fixture();for(let i=0;i<100;i++)f.view.update();assert.equal(getEventListeners(f.abort.signal,'abort').length,0);
 const tab=f.buttons.filter(b=>b.className==='session-tab').at(-1),down=new Event('pointerdown');down.button=0;tab.dispatchEvent(down);assert.equal(f.timers.size,1);
 f.view.update();assert.equal(f.timers.size,0);f.advance(500);assert.equal(f.sheets.length,0);
 let stop;for(let i=0;i<100;i++){stop?.();stop=f.longPress(new Element('button'),()=>{});assert.equal(getEventListeners(f.abort.signal,'abort').length,1);}
 stop();assert.equal(getEventListeners(f.abort.signal,'abort').length,0);f.view.cleanup();
 console.log('PASS longPress abort listener cleanup across 100 redraws');
 f=fixture([{id:7,agent:'claude',state:'Waiting'},{id:8,agent:'claude',state:'Working'},{id:9,agent:null,state:'Unknown'},{id:10,agent:'antigravity',state:'Done'},{id:11,agent:'my-bot',state:'Working',exited:true}]);
 const tabs=()=>f.buttons.filter(b=>b.className==='session-tab').slice(-5),summary=()=>tabs().map(t=>[t.children[0].className,t.children[1].attrs.class||t.children[1].className,t.children[2].textContent,t.attrs['aria-label']]);
 assert.deepEqual(summary(),[['status-dot waiting','agent-icon claude','Claude Code','Claude Code · Waiting · Terminal #7'],['status-dot working','agent-icon claude','Claude Code 2','Claude Code 2 · Working · Terminal #8'],['status-dot unknown','agent-icon','Shell','Shell · Unknown · Terminal #9'],['status-dot done','agent-icon agy','Antigravity','Antigravity · Done · Terminal #10'],['status-dot exited','agent-icon custom','my-bot','my-bot · Exited · Terminal #11']]);
 assert.equal(tabs()[0].children[1].children.length,1);assert.equal(tabs()[3].children[1].children.at(-1).attrs.class,'layer');assert.equal(tabs()[4].children[1].textContent,'M');
 const strip=tabs()[0].parentElement;strip.scrollLeft=300;f.state.sessions.get(8).state='Waiting';f.view.update();assert.equal(tabs()[1].children[0].className,'status-dot waiting');assert.equal(tabs()[0].parentElement.scrollLeft,300);
 f.buttons.find(b=>b.textContent==='Terminal actions').action();assert(f.sheets.at(-1).some(e=>e.label==='Select text'));f.view.cleanup();
 f=fixture([{id:7,agent:null,state:'Unknown'},{id:8,agent:'codex',state:'Working'}]);f.mounts[0].options.onStatus({phase:'ready'});f.mounts[0].options.onTitle('✳ vim notes.md');assert.deepEqual(tabs().slice(-2).map(t=>t.children[2].textContent),['vim notes.md','Codex']);
 f.mounts[0].options.onStatus({phase:'disconnected',typing:true});f.advance(1000);assert.equal(f.mounts.length,2);assert.equal(f.mounts[1].focused,0);f.mounts[1].options.onStatus({phase:'ready'});assert.equal(f.mounts[1].focused,1);f.mounts[1].options.onStatus({phase:'ready'});assert.equal(f.mounts[1].focused,1);f.view.cleanup();
 console.log('PASS OSC title labels agentless tabs, input refocus after reconnect only when typing');
 console.log('PASS tab provider mark/name, state dot, duplicate numbering and tab-strip scroll kept across updates');
 await testHybridChatToggleAndQuestionCards();
 console.log('PASS Hybrid Chat [Chat | Term] toggle, chat_poll cards, question answer_question, and diff modal');
}
async function testHybridChatToggleAndQuestionCards(){
 const timers=new Map(),mounts=[],unmounts=[],buttons=[],sheets=[],calls=[],abort=new AbortController();let clock=0,id=0;
 const sandbox={AbortController,abort,document:{createElementNS:(_,tag)=>new Element(tag)},setTimeout:(fn,ms)=>{timers.set(++id,{fn,at:clock+ms});return id;},clearTimeout:id=>timers.delete(id),KrillMobile:{terminal:{mount(root,options){const h={options,unmount(){unmounts.push(options.sessionId);}};mounts.push(h);options.onStatus({phase:'ready'});return h;}}}};
 vm.createContext(sandbox);
 vm.runInContext(longPressCode,sandbox);
 vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/chat.js'),'utf8'),sandbox);
 vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/session.js'),'utf8'),sandbox);
 const session={id:7,state:'Working',agent:'claude',branch:'main',lines:['● Editing src/lib.rs']},shellSession={id:8,state:'Idle',agent:'',branch:'main',lines:['$']},group={key:'g',label:'Workspace',sessions:[session,shellSession]},state={groups:new Map([['g',group]]),sessions:new Map([[7,session],[8,shellSession]]),connection:{phase:'ready'},preferences:{},drafts:new Map()};
 const el=(...args)=>new Element(...args);
 const ctx={el,state,route:{groupKey:'g',sessionId:7},signal:abort.signal,button(label,action,cls){const b=el('button',label,cls);b.action=action;buttons.push(b);return b;},header(){const h=el('header'),copy=el('div');copy.append(el('h1'));h.append(copy);return h;},icon:()=>el('svg'),dot:status=>el('span',undefined,`status-dot ${String(status).toLowerCase()}`),longPress:sandbox.longPress,groupRoute:()=>'',go(){},rerender(){},savePreferences(p){Object.assign(state.preferences,p);},error(){},photo(){},transport:{},sheet:(title,entries,_,render)=>sheets.push({title,entries,render}),act:fn=>fn(),rpc:async req=>{calls.push(req);if(req.action==='chat_poll')return {agent:'claude',bound:true,next_offset:128,reset:false,items:[{kind:'user',text:'Fix bug'},{kind:'tool_call',tool_name:'Edit',title:'Update lib',file_path:'src/lib.rs',text:'src/lib.rs'},{kind:'question',title:'Mode',text:'Pick one',multi_select:false,options:[{label:'Fast',description:'Quick'},{label:'Safe',description:'Thorough'}]}]};if(req.action==='git')return {patch:'+added line'};return {sent:true};},refresh:async()=>{}};
 const root=el('main');
 const view=sandbox.KrillMobile.screens.session(root,ctx);
 assert.equal(mounts.length,1);
 const flush=async()=>{for(let i=0;i<10;i++)await Promise.resolve();};
 const toggleBtn=buttons.find(b=>b.className?.includes('mode-toggle'));
 // 기본 보기는 에이전트 세션(claude)도 터미널이다. Chat은 사용자가 고르면 열리고 세션별로 저장된다.
 assert.equal(toggleBtn.textContent,'Chat');
 await flush();assert(!calls.some(c=>c.action==='chat_poll'));
 toggleBtn.action();assert.equal(toggleBtn.textContent,'Term');assert.equal(state.preferences.sessionViewModes[7],'chat');
 await flush();
 assert(calls.some(c=>c.action==='chat_poll'&&c.session===7));
 const fastOpt=buttons.find(b=>b.textContent?.startsWith('Fast'));
 assert(fastOpt);
 fastOpt.action();await flush();
 assert(calls.some(c=>c.action==='answer_question'&&c.session===7&&c.selected[0]===0));
 const diffBtn=buttons.find(b=>b.textContent==='Diff');
 assert(diffBtn);
 diffBtn.action();await flush();
 assert(calls.some(c=>c.action==='git'&&c.path==='src/lib.rs'));
 const rawBtn=buttons.find(b=>b.className?.includes('chat-raw-btn'));
 rawBtn.action();
 assert.equal(toggleBtn.textContent,'Chat');
 // [Chat | Term] 전환 시 TerminalAdapter를 언마운트하지 않고 그대로 유지해야 한다(0ms 전환).
 assert.equal(mounts.length,1);
 assert.equal(unmounts.length,0);
 toggleBtn.action();
 assert.equal(toggleBtn.textContent,'Term');
 assert.equal(mounts.length,1);
 assert.equal(unmounts.length,0);
 view.cleanup();
 assert.equal(unmounts.length,1);
 buttons.length=0;const savedView=sandbox.KrillMobile.screens.session(el('main'),ctx);assert.equal(buttons.find(b=>b.className?.includes('mode-toggle')).textContent,'Term');savedView.cleanup();
 // 일반 셸 세션(agent === '')은 기본값이 Term 뷰다.
 buttons.length=0;
 const shellView=sandbox.KrillMobile.screens.session(el('main'),{...ctx,route:{groupKey:'g',sessionId:8}});
 const shellToggle=buttons.find(b=>b.className?.includes('mode-toggle'));
 assert.equal(shellToggle.textContent,'Chat');
 shellView.cleanup();
}
async function testTransportAuthRetries(){
 let now=1000,nextId=0;const timers=new Map(),sockets=[],statuses=[],errors=[];
 class FakeSocket extends EventTarget{
  constructor(){super();this.sent=[];sockets.push(this);queueMicrotask(()=>this.dispatchEvent(new Event('open')));}
  send(data){this.sent.push(data);}
  close(code=1006){if(this.closed)return;this.closed=true;const e=new Event('close');e.code=code;this.dispatchEvent(e);}
  message(data){const e=new Event('message');e.data=data;this.dispatchEvent(e);}
 }
 const sandbox={Event,EventTarget,TextEncoder,TextDecoder,Uint8Array,DataView,BigInt,Promise,Math,Error,JSON,Date:{now:()=>now},crypto:{getRandomValues:b=>b.fill(1)},setTimeout:(fn,ms)=>{const id=++nextId;timers.set(id,{fn,at:now+Math.max(0,ms)});return id;},clearTimeout:id=>timers.delete(id),WebSocket:FakeSocket,MkCrypto:{enc:new TextEncoder(),concat:(...a)=>{const out=new Uint8Array(a.reduce((n,x)=>n+x.length,0));let i=0;for(const x of a){out.set(x,i);i+=x.length;}return out;},unhex:()=>new Uint8Array(32),hex:()=>'00'.repeat(32),sha256:a=>a,unbase64:()=>new Uint8Array(32),Channel:class{seal(a){return a;}open(a){return a;}}},KrillMobile:{}};
 vm.createContext(sandbox);vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/client.js'),'utf8'),sandbox);
 const flush=async()=>{for(let i=0;i<10;i++)await Promise.resolve();};
 const advance=async ms=>{now+=ms;for(const [id,t] of [...timers])if(t.at<=now){timers.delete(id);t.fn();await flush();}};
 const t=sandbox.KrillMobile.transport.create({url:'ws://localhost/ws',credentials:{device_id:'dev1',credential:'aa'.repeat(32)},onStatus:s=>statuses.push(s.phase),onError:e=>errors.push(e.message)});
 t.connect();await flush();assert.equal(sockets.length,1);
 sockets[0].message(JSON.stringify({version:2,challenge:'bb'.repeat(32)}));await flush();
 sockets[0].close(1006);await flush();
 assert.equal(timers.size,1);
 t.resume();await flush();assert.equal(sockets.length,1);
 await advance(64999);assert.equal(sockets.length,1);
 await advance(1);assert.equal(sockets.length,2);
 sockets[1].message(JSON.stringify({version:2,challenge:'bb'.repeat(32)}));await flush();
 sockets[1].close(1006);await flush();
 assert.equal(timers.size,1);
 await advance(129999);assert.equal(sockets.length,2);
 await advance(1);assert.equal(sockets.length,3);
 sockets[2].message(JSON.stringify({version:2,challenge:'bb'.repeat(32)}));await flush();
 sockets[2].close(1006);await flush();
 assert.equal(timers.size,0);
 t.resume();await flush();assert.equal(sockets.length,3);
 t.reconnect();await flush();assert.equal(sockets.length,4);
 sockets[3].message(JSON.stringify({version:2,challenge:'bb'.repeat(32)}));await flush();
 sockets[3].close(1008);await flush();
 assert.equal(timers.size,0);assert.match(errors.at(-1),/Desktop rejected authentication/);
 await advance(200000);t.resume();await flush();assert.equal(sockets.length,4);
 t.close();
}
async function testPipelinedHandshakeAndPolling(){
 let now=0,nextId=0;const timers=new Map(),sockets=[],snapshots=[],statuses=[];
 class FakeSocket extends EventTarget{
  constructor(){super();this.readyState=1;this.sent=[];sockets.push(this);queueMicrotask(()=>this.dispatchEvent(new Event('open')));}
  send(data){this.sent.push(data);}
  close(){this.closed=true;this.readyState=3;this.dispatchEvent(new Event('close'));}
  message(data){const e=new Event('message');e.data=data;this.dispatchEvent(e);}
 }
 const document={hidden:false},enc=new TextEncoder(),concat=(...a)=>{const out=new Uint8Array(a.reduce((n,x)=>n+x.length,0));let i=0;for(const x of a){out.set(x,i);i+=x.length;}return out;};
 const sandbox={Event,EventTarget,TextEncoder,TextDecoder,Uint8Array,DataView,BigInt,Promise,Math,Error,JSON,Number,document,Date:{now:()=>now},crypto:{getRandomValues:b=>b.fill(1)},setTimeout:(fn,ms)=>{const id=++nextId;timers.set(id,{fn,at:now+Math.max(0,ms)});return id;},clearTimeout:id=>timers.delete(id),WebSocket:FakeSocket,MkCrypto:{enc,concat,unhex:()=>new Uint8Array(32),hex:()=>'00'.repeat(32),sha256:a=>a,unbase64:()=>new Uint8Array(32),Channel:class{seal(a){return a;}open(a){return new Uint8Array(a);}}},KrillMobile:{}};
 vm.createContext(sandbox);vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/client.js'),'utf8'),sandbox);
 const flush=()=>new Promise(r=>setImmediate(r));
 const frame=(kind,body)=>{const h=new Uint8Array(5);new DataView(h.buffer).setUint32(0,body.length+1,true);h[4]=kind;return concat(h,body);};
 const text=s=>{const b=enc.encode(s);return concat(Uint8Array.of(b.length),b);};
 const errors=[];
 const t=sandbox.KrillMobile.transport.create({url:'ws://localhost/ws',credentials:{device_id:'dev1',credential:'aa'.repeat(32)},onSnapshot:d=>snapshots.push(d),onStatus:s=>statuses.push(s.phase),onError:e=>errors.push(e.message)});
 t.connect();await flush();const socket=sockets[0];socket.message(JSON.stringify({version:2,challenge:'bb'.repeat(32)}));await flush();
 // 인증 응답 전에 auth hello·proof·ClientHello·snapshot 요청이 모두 나가야 한다.
 assert.equal(socket.sent.length,4);assert.equal(socket.sent[2][4],2);assert.equal(socket.sent[3][4],0x45);
 const reply=()=>socket.message(frame(0x65,concat(Uint8Array.of(6),text(JSON.stringify({ok:true,data:{sessions:[]}})))));
 socket.message(enc.encode('{"device_id":"dev1"}'));socket.message(frame(1,Uint8Array.of(1,1,0,0,1)));reply();await flush();
 assert.equal(snapshots.length,1);assert.equal(statuses.at(-1),'ready');assert.equal(socket.sent.length,4);
 // 컨트롤 소켓 ready 직후 백그라운드 터미널 WebSocket 1개가 프리워밍되며 challenge 수신 시 즉시 인증·ClientHello까지 마친다.
 assert.equal(sockets.length,2);
 const warmSocket=sockets[1];warmSocket.message(JSON.stringify({version:2,challenge:'cc'.repeat(32)}));
 assert.equal(warmSocket.sent.length,3);assert.equal(warmSocket.sent[2][4],2);
 warmSocket.message(enc.encode('{"device_id":"dev1"}'));warmSocket.message(frame(1,Uint8Array.of(1,1,0,0,1)));
 const claimed=t.terminalConnect();assert.equal(claimed.socket,warmSocket);assert(claimed.channel);assert.equal(claimed.messages.length,2);
 const fresh=t.terminalConnect();assert.notEqual(fresh.socket,warmSocket);assert.equal(sockets.length,3);
 const [[,poll]]=[...timers];assert.equal(poll.at,2500);
 document.hidden=true;now=2500;timers.clear();poll.fn();await flush();assert.equal(socket.sent.length,4);
 sandbox.KrillMobile.activeTerminal={};document.hidden=false;const [[,next]]=[...timers];timers.clear();next.fn();await flush();assert.equal(socket.sent.length,5);reply();await flush();assert.equal(snapshots.length,2);
 const [[,slow]]=[...timers];assert.equal(slow.at-now,10000);
 // resume() 시 소진된 터미널 프리워밍 소켓을 다시 채우고, 정상 응답이 1.5초 안에 오면 재연결하지 않는다.
 t.resume();await flush();assert.equal(sockets.length,4);assert.equal(socket.sent.length,6);reply();await flush();assert.equal(snapshots.length,3);
 for(const [id,tm] of [...timers])if(tm.at<=now+1500){timers.delete(id);tm.fn();}await flush();assert.equal(sockets.length,4);
 // 1.5초 동안 응답이 없으면 좀비 소켓으로 판정해 즉시 reconnect()하며, 끊긴 refresh의 오류를 onError로 흘리지 않는다.
 t.resume();await flush();assert.equal(socket.sent.length,7);
 const zombieTimer=[...timers.values()].find(tm=>tm.at===now+1500);assert(zombieTimer);
 zombieTimer.fn();await flush();
 assert.equal(socket.closed,true);assert.equal(sockets.length,5);assert.deepEqual(errors,[]);
 t.close();
}
if(require.main===module)main().catch(e=>{console.error(e);process.exitCode=1;});
module.exports={fixture,main};
