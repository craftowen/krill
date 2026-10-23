// Session screen regressions without a browser or a running app.
'use strict';
const assert=require('node:assert/strict'),{getEventListeners}=require('node:events'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
class Element extends EventTarget {
 constructor(tag,text,cls){super();this.tag=tag;this.textContent=text;this.className=cls;this.children=[];this.dataset={};this.attrs={};this.style={};this.scrollLeft=0;this.classList={toggle:()=>{},remove:()=>{}};}
 append(...nodes){for(const n of nodes){n.parentElement=this;this.children.push(n);}}
 insertBefore(node,before){if(node===before)return;node.remove();const index=before?this.children.indexOf(before):-1;node.parentElement=this;if(index<0)this.children.push(node);else this.children.splice(index,0,node);}
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
function fixture(sessions=[{id:7,state:'Working',branch:null}],snapshot,tablet=false){
 const timers=new Map(),mounts=[],buttons=[],sheets=[],calls=[],routes=[],remembered=[],abort=new AbortController(),wide=new EventTarget();wide.matches=tablet;let clock=0,id=0;
 let resize;const sandbox={ResizeObserver:class{constructor(fn){resize=fn;}observe(){}disconnect(){}},AbortController,abort,matchMedia:()=>wide,document:{createElementNS:(_,tag)=>new Element(tag)},setTimeout:(fn,ms)=>{timers.set(++id,{fn,at:clock+ms});return id;},clearTimeout:id=>timers.delete(id),KrillMobile:{terminal:{mount(root,options){const h={options,focused:0,unmounted:false,fitToPhone:()=>Promise.resolve(),setInputActive(active){h.inputActive=active;},focus(){h.focused++;},unmount(){h.unmounted=true;options.onStatus({phase:'disconnected',message:'Disconnected'});}};mounts.push(h);options.onStatus({phase:'connecting'});return h;}}}};
 vm.createContext(sandbox);vm.runInContext(longPressCode,sandbox);vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/session.js'),'utf8'),sandbox);
 const group={key:'g',label:'Workspace',sessions},state={groups:new Map([['g',group]]),sessions:new Map(sessions.map(s=>[s.id,s])),connection:{phase:'ready'},preferences:{},drafts:new Map(),snapshot};
 const el=(...args)=>new Element(...args),ctx={el,state,route:{groupKey:'g',sessionId:7},signal:abort.signal,button(label,action,cls){const b=el('button',label,cls);b.action=action;buttons.push(b);return b;},header(){const h=el('header'),copy=el('div');copy.append(el('h1'));h.append(copy);return h;},icon:()=>el('svg'),dot:status=>el('span',undefined,`status-dot ${String(status).toLowerCase()}`),longPress:sandbox.longPress,groupRoute:()=>'',go(r){routes.push(r);},rerender(){},savePreferences(p){Object.assign(state.preferences,p);},rememberSession:(s,g)=>remembered.push([s.id,g.key]),error(){},photo(){},transport:{},sheet:(title,entries,_,render)=>{entries.title=title;entries.render=render;sheets.push(entries);},closeSheet(){},rpc:async request=>calls.push(request),refresh:async()=>{}};
 const root=el('main'),view=sandbox.KrillMobile.screens.session(root,ctx);
 function advance(ms){clock+=ms;for(const [key,t] of [...timers])if(t.at<=clock){timers.delete(key);t.fn();}}
 return {resize:width=>resize([{contentRect:{width}}]),view,state,ctx,abort,wide,root,sandbox,longPress:sandbox.longPress,mounts,timers,advance,buttons,sheets,calls,routes,remembered};
}
async function main(){
 {const f=fixture([{id:7,state:'Working'},{id:8,state:'Working'}],undefined,true),M=f.sandbox.KrillMobile;
  const width=value=>{f.wide.matches=value;f.wide.dispatchEvent(new Event('change'));};
  const first=f.mounts[0];first.options.onStatus({phase:'ready'});
  f.buttons.find(b=>b.textContent==='Split terminal').action();assert.equal(f.sheets.at(-1).length,1);assert(f.sheets.at(-1)[0].label.endsWith('#8'));f.sheets.at(-1)[0].action();
  const second=f.mounts[1];assert.equal(M.activeTerminal,second);assert.equal(second.options.preferences.fitToPhone,true);assert.equal(first.inputActive,false);assert.equal(second.inputActive,true);
  width(false);assert(first.unmounted);assert(!second.unmounted);assert.equal(f.timers.size,0);assert.equal(M.activeTerminal,second);
  width(true);const restored=f.mounts[2];assert.equal(restored.options.sessionId,7);assert.equal(M.activeTerminal,second);
  f.root.descendants().find(n=>n.className==='terminal-pane primary-pane').dispatchEvent(new Event('pointerdown'));
  width(false);assert(!restored.unmounted);assert(second.unmounted);assert.equal(M.activeTerminal,restored);
  width(true);const next=f.mounts.at(-1);next.options.onStatus({phase:'disconnected'});assert.equal(f.timers.size,1);
  f.state.connection.phase='disconnected';f.view.update();f.advance(1000);const count=f.mounts.length;assert(next.unmounted);assert.equal(f.timers.size,0);
  f.state.connection.phase='ready';f.view.update();assert.equal(f.mounts.length,count+1);assert.equal(f.mounts.at(-1).options.sessionId,8);
  f.buttons.find(b=>b.textContent==='Close pane').action();assert(f.mounts.at(-1).unmounted);assert(!restored.unmounted);assert.equal(f.calls.length,0);assert.equal(f.state.preferences.tabletSplits.tabletSplit,null);f.view.cleanup();
  assert(f.mounts.every(h=>h.unmounted));assert.equal(f.timers.size,0);
 }
 {const f=fixture([{id:7,state:'Working'},{id:8,state:'Working'}],undefined,true);
  f.buttons.find(b=>b.textContent==='Split terminal').action();f.sheets.at(-1)[0].action();
  const panels=f.root.descendants().find(n=>n.className==='session-panels'),control=f.buttons.find(b=>b.className==='ghost split-direction');
  const select=value=>{control.action();const body=new Element('div');f.sheets.at(-1).render(body);const choices=body.descendants().filter(n=>n.dataset.direction);assert.equal(choices.filter(n=>n.attrs['aria-pressed']==='true').length,1);choices.find(n=>n.dataset.direction===value).action();};
  f.resize(799);assert.equal(panels.dataset.layout,'rows');f.resize(800);assert.equal(panels.dataset.layout,'columns');
  select('rows');f.resize(1200);assert.equal(panels.dataset.layout,'rows');assert.equal(f.state.preferences.tabletSplits.tabletSplit.direction,'rows');
  select('columns');f.resize(720);assert.equal(panels.dataset.layout,'rows');assert.equal(control.dataset.fallback,'true');f.resize(721);assert.equal(panels.dataset.layout,'columns');assert.equal(control.dataset.fallback,'false');
  select('auto');assert.equal(panels.dataset.layout,'rows');f.view.cleanup();
 }
 console.log('PASS split direction selection, current choice, persisted state, automatic threshold and minimum pane width');
 console.log('PASS tablet split excludes duplicate PTY, preserves active pane on folding, reconnects after main transport and closes only the view');
 let f=fixture();assert.deepEqual(f.remembered.at(-1),[7,'g']);assert.equal(f.mounts.length,1);f.mounts[0].options.onStatus({phase:'ready'});f.mounts[0].options.onStatus({phase:'disconnected'});for(let n=0;n<10;n++)f.view.update();assert.equal(f.timers.size,1);f.advance(999);assert.equal(f.mounts.length,1);f.advance(1);assert.equal(f.mounts.length,2);f.mounts[1].options.onStatus({phase:'disconnected'});f.advance(1999);assert.equal(f.mounts.length,2);f.advance(1);assert.equal(f.mounts.length,3);f.mounts[2].options.onStatus({phase:'disconnected'});f.view.cleanup();f.advance(60000);assert.equal(f.mounts.length,3);f.mounts[0].options.onStatus({phase:'disconnected'});assert.equal(f.timers.size,0);
 f=fixture();f.mounts[0].options.onStatus({phase:'disconnected'});f.state.connection.phase='disconnected';f.view.update();f.advance(60000);assert.equal(f.mounts.length,1);f.state.connection.phase='ready';f.view.update();assert.equal(f.mounts.length,2);f.view.cleanup();
 console.log('PASS terminal-only reconnect backoff, generation/cleanup and main connection cancellation');
 f=fixture();const actions=f.buttons.find(b=>b.textContent==='Terminal actions'),git=f.buttons.find(b=>b.textContent==='Git changes');assert(git.hidden);
 const replace=(state,branch=null)=>{const session={id:7,state,branch};f.state.sessions.set(7,session);f.state.groups.set('g',{key:'g',label:'Latest workspace',sessions:[session]});};
 replace('Waiting','main');actions.action();let entries=f.sheets.at(-1);assert(entries.some(e=>e.label==='Review in Terminal'));assert(!entries.some(e=>e.label==='Approve (Yes)'||e.label==='Deny (No)'));f.view.update();assert.equal(git.hidden,false);
 replace('Working');entries.find(e=>e.label==='Review in Terminal').action();assert.equal(f.calls.length,0);assert.equal(f.mounts[0].focused,1);actions.action();assert(!f.sheets.at(-1).some(e=>e.label==='Review in Terminal'));f.view.update();assert(git.hidden);
 replace('Blocked');actions.action();entries=f.sheets.at(-1);entries.find(e=>e.label==='Review in Terminal').action();assert.equal(f.calls.length,0);f.state.sessions.delete(7);assert.throws(()=>entries.find(e=>e.label==='Review in Terminal').action(),/no longer available/);f.view.cleanup();
 console.log('PASS latest approval context opens Terminal review without unchecked approve RPC, stale target rejection and branch controls');
 f=fixture([{id:7,state:'Waiting',branch:null,lines:['Do you want to proceed?','❯ 1. Yes']}]);{let inserted,reveal;f.mounts.at(-1).loadHistory=r=>{reveal=r;};f.buttons.find(b=>b.textContent==='Terminal actions').action();const sheet=f.sheets.at(-1);sheet.render({querySelector:s=>s==='h2'?{after:n=>{inserted=n;}}:null});assert.equal(inserted.className,'sheet-context');assert.equal(inserted.textContent,'Do you want to proceed?\n❯ 1. Yes');await sheet.find(e=>e.label==='Load earlier output').action();assert.equal(reveal,true);
  f.state.sessions.get(7).state='Working';f.buttons.find(b=>b.textContent==='Terminal actions').action();assert.equal(f.sheets.at(-1).render,undefined);f.view.cleanup();}
 console.log('PASS approval sheet shows latest lines above actions; Load earlier output reveals history');
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
 f=fixture([{id:7,agent:'',foreground:'/opt/bin/Codex',title:'모바일 탭 전환 수정',state:'Working'},{id:8,agent:'claude',title:'✳ 로그인 오류 수정',state:'Waiting'},{id:9,agent:'my-bot',foreground:'codex',title:'Custom task',state:'Working'}]);
 let identityTabs=f.buttons.filter(b=>b.className==='session-tab').slice(-3);
 assert.deepEqual(identityTabs.map(t=>[t.children[1].attrs.class||t.children[1].className,t.children[2].textContent]),[['agent-icon codex','모바일 탭 전환 수정'],['agent-icon claude','로그인 오류 수정'],['agent-icon custom','Custom task']]);
 f.mounts[0].options.onTitle('Codex');assert.equal(f.buttons.filter(b=>b.className==='session-tab').slice(-3)[0].children[2].textContent,'모바일 탭 전환 수정');
 f.state.sessions.get(7).title='새 작업 제목';f.view.update();identityTabs=f.buttons.filter(b=>b.className==='session-tab').slice(-3);assert.equal(identityTabs[0].children[2].textContent,'새 작업 제목');
 f.buttons.find(b=>b.textContent==='Terminal actions').action();assert.equal(f.sheets.at(-1).title,'새 작업 제목');f.view.cleanup();
 f=fixture([{id:7,agent:'zsh',title:'Codex task',state:'Working'}]);assert.equal(f.buttons.find(b=>b.className==='session-tab').children[1].attrs.class,'agent-icon codex');f.view.cleanup();
 console.log('PASS snapshot titles before visiting tabs, foreground provider fallback, hook/custom identity precedence and title updates');
 console.log('PASS OSC title labels agentless tabs, input refocus after reconnect only when typing');
 console.log('PASS tab provider mark/name, state dot, duplicate numbering and tab-strip scroll kept across updates');
 await testPromptDraftAndInactiveMenu();
 await testHybridChatToggleAndQuestionCards();
 console.log('PASS Hybrid Chat visibility, chat_poll cards, historical answers remain notes until Terminal review, and current diff modal');
}
async function testPromptDraftAndInactiveMenu(){
 const flush=async()=>{for(let i=0;i<12;i++)await Promise.resolve();},event=(el,type,props={})=>{const e=new Event(type,{cancelable:true});Object.assign(e,props);el.dispatchEvent(e);return e;};
 let f=fixture(),closed=0,resolve;f.ctx.closeSheet=()=>closed++;f.ctx.act=fn=>fn();f.ctx.rpc=req=>{f.calls.push(req);return new Promise(r=>resolve=r);};
 let activeBody;const open=()=>{f.buttons.find(b=>b.textContent==='Terminal actions').action();f.sheets.at(-1).find(e=>e.label==='Send prompt').action();const body=activeBody=new Element('div');f.sheets.at(-1).render(body);const form=body.children[0];return {form,input:form.children[0],send:form.children[1]};};
 let {form,input,send}=open();input.value='first';event(input,'input');event(form,'submit');event(form,'submit');assert.equal(f.calls.length,1);assert.equal(send.disabled,true);input.value='second';event(input,'input');resolve({sent:true});await flush();assert.equal(input.value,'second');assert.equal(f.state.drafts.get(7),'second');assert.equal(closed,0);assert.equal(send.disabled,false);
 // Even editing back to the sent value is a new draft revision.
 event(form,'submit');input.value='other';event(input,'input');input.value='second';event(input,'input');resolve({sent:true});await flush();assert.equal(f.state.drafts.get(7),'second');assert.equal(closed,0);
 event(form,'submit');const oldResolve=resolve;({form,input,send}=open());input.value='new sheet draft';event(input,'input');oldResolve({sent:true});await flush();assert.equal(f.state.drafts.get(7),'new sheet draft');assert.equal(closed,0);
 event(form,'submit');resolve({sent:true});await flush();assert.equal(f.state.drafts.has(7),false);assert.equal(input.value,'');assert.equal(closed,1);
 input.value='closed sheet';event(input,'input');event(form,'submit');activeBody.closest=()=>({open:false});f.state.drafts.set(7,'closed sheet');resolve({sent:true});await flush();assert.equal(f.state.drafts.get(7),'closed sheet');assert.equal(closed,1);
 input.value='retain after cleanup';event(input,'input');event(form,'submit');f.view.cleanup();resolve({sent:true});await flush();assert.equal(f.state.drafts.get(7),'retain after cleanup');assert.equal(closed,1);
 console.log('PASS TR-A02 prompt sheet single submit, versioned edits, replacement sheet, successful clear and unmounted draft protection');
 f=fixture([{id:7,state:'Working'},{id:8,state:'Waiting'}]);f.sandbox.KrillMobile.chat={mount(){return {};}};f.ctx.groupRoute=(_,id)=>`/s/${id}`;f.buttons.find(b=>b.textContent==='Terminal actions').action();assert.equal(f.sheets.at(-1)[0].label,'Switch to Chat');const tab=f.buttons.filter(b=>b.className==='session-tab').at(-1);event(tab,'contextmenu');const menu=f.sheets.at(-1);assert(!menu.some(e=>/^Switch to /.test(e.label)));menu.find(e=>e.label==='Review in Terminal').action();assert.equal(f.routes.at(-1),'/s/8');assert.equal(f.calls.length,0);f.view.cleanup();
 console.log('PASS TR-A13 inactive tab menu cannot switch active view and approval review routes to the targeted tab');
}
async function testHybridChatToggleAndQuestionCards(){
 const timers=new Map(),mounts=[],unmounts=[],buttons=[],sheets=[],calls=[],abort=new AbortController();let clock=0,id=0;
 const sandbox={AbortController,abort,document:{createElementNS:(_,tag)=>new Element(tag)},setTimeout:(fn,ms)=>{timers.set(++id,{fn,at:clock+ms});return id;},clearTimeout:id=>timers.delete(id),KrillMobile:{terminal:{mount(root,options){const h={options,visible:options.visible,setVisible(v){h.visible=v;},unmount(){unmounts.push(options.sessionId);}};mounts.push(h);options.onStatus({phase:'ready'});return h;}}}};
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
 buttons.find(b=>b.textContent==='Terminal actions').action();assert.equal(sheets.at(-1).entries[0].label,'Switch to Chat');sheets.at(-1).entries[0].action();assert.equal(toggleBtn.textContent,'Term');assert.equal(toggleBtn.getAttribute('aria-label'),'Term');assert.equal(state.preferences.sessionViewModes[7],'chat');assert.equal(mounts[0].visible,false);
 await flush();
 assert(calls.some(c=>c.action==='chat_poll'&&c.session===7));
 const fastOpt=buttons.find(b=>b.textContent?.startsWith('Fast'));
 assert(fastOpt);
 fastOpt.action();await flush();
 assert(!calls.some(c=>c.action==='answer_question'||c.action==='approve'));
 const diffBtn=buttons.find(b=>b.textContent==='Current diff');
 assert(diffBtn);
 diffBtn.action();await flush();
 assert(calls.some(c=>c.action==='git'&&c.path==='src/lib.rs'));
 const rawBtn=buttons.find(b=>b.className?.includes('chat-raw-btn'));
 buttons.find(b=>b.textContent==='Terminal actions').action();assert.equal(sheets.at(-1).entries[0].label,'Switch to Terminal');sheets.at(-1).entries[0].action();
 assert.equal(toggleBtn.textContent,'Chat');assert.equal(mounts[0].visible,true);
 // [Chat | Term] 전환 시 TerminalAdapter를 언마운트하지 않고 그대로 유지해야 한다(0ms 전환).
 assert.equal(mounts.length,1);
 assert.equal(unmounts.length,0);
 toggleBtn.action();
 assert.equal(toggleBtn.textContent,'Term');
 rawBtn.action();assert.equal(toggleBtn.textContent,'Chat');toggleBtn.action();
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
 const sandbox={AbortController,Event,EventTarget,TextEncoder,TextDecoder,Uint8Array,DataView,BigInt,Promise,Math,Error,JSON,Date:{now:()=>now},crypto:{getRandomValues:b=>b.fill(1)},setTimeout:(fn,ms)=>{const id=++nextId;timers.set(id,{fn,at:now+Math.max(0,ms)});return id;},clearTimeout:id=>timers.delete(id),WebSocket:FakeSocket,MkCrypto:{enc:new TextEncoder(),concat:(...a)=>{const out=new Uint8Array(a.reduce((n,x)=>n+x.length,0));let i=0;for(const x of a){out.set(x,i);i+=x.length;}return out;},unhex:()=>new Uint8Array(32),hex:()=>'00'.repeat(32),sha256:a=>a,unbase64:()=>new Uint8Array(32),Channel:class{seal(a){return a;}open(a){return a;}}},KrillMobile:{}};
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
 t.reconnect();await flush();assert.equal(sockets.length,3);await advance(t.state.retryAt-now);assert.equal(sockets.length,4);
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
 const sandbox={AbortController,Event,EventTarget,TextEncoder,TextDecoder,Uint8Array,DataView,BigInt,Promise,Math,Error,JSON,Number,document,Date:{now:()=>now},crypto:{getRandomValues:b=>b.fill(1)},setTimeout:(fn,ms)=>{const id=++nextId;timers.set(id,{fn,at:now+Math.max(0,ms)});return id;},clearTimeout:id=>timers.delete(id),WebSocket:FakeSocket,MkCrypto:{enc,concat,unhex:()=>new Uint8Array(32),hex:()=>'00'.repeat(32),sha256:a=>a,unbase64:()=>new Uint8Array(32),Channel:class{seal(a){return a;}open(a){return new Uint8Array(a);}}},KrillMobile:{}};
 vm.createContext(sandbox);vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/client.js'),'utf8'),sandbox);
 const flush=()=>new Promise(r=>setImmediate(r));
 const frame=(kind,body)=>{const h=new Uint8Array(5);new DataView(h.buffer).setUint32(0,body.length+1,true);h[4]=kind;return concat(h,body);};
 const text=s=>{const b=enc.encode(s);return concat(Uint8Array.of(b.length),b);};
 const errors=[];
 const t=sandbox.KrillMobile.transport.create({url:'ws://localhost/ws',credentials:{device_id:'dev1',credential:'aa'.repeat(32)},onSnapshot:d=>snapshots.push(d),onStatus:s=>statuses.push(s.phase),onError:e=>errors.push(e.message)});
 t.connect();await flush();const socket=sockets[0];socket.message(JSON.stringify({version:2,challenge:'bb'.repeat(32)}));await flush();
 // 인증 응답 전에 auth hello·proof·ClientHello·snapshot 요청이 모두 나가야 한다.
 assert.equal(socket.sent.length,4);assert.equal(socket.sent[2][4],2);assert.equal(socket.sent[3][4],0x45);
 const reply=(data={sessions:[]})=>socket.message(frame(0x65,concat(Uint8Array.of(6),text(JSON.stringify({ok:true,data})))));
 socket.message(enc.encode('{"device_id":"dev1"}'));socket.message(frame(1,Uint8Array.of(1,1,0,0,1)));reply({sessions:[],etag:'e1'});await flush();
 assert.equal(snapshots.length,1);assert.equal(statuses.at(-1),'ready');assert.equal(socket.sent.length,4);
 // 컨트롤 소켓 ready 직후 백그라운드 터미널 WebSocket 1개가 프리워밍되며 challenge 수신 시 즉시 인증·ClientHello까지 마친다.
 assert.equal(sockets.length,2);
 const warmSocket=sockets[1];warmSocket.message(JSON.stringify({version:2,challenge:'cc'.repeat(32)}));
 assert.equal(warmSocket.sent.length,3);assert.equal(warmSocket.sent[2][4],2);
 const warmHello=warmSocket.sent[2].slice(5);let warmFeatures=0n,warmShift=0n;for(let at=2;at<warmHello.length;at++){const byte=warmHello[at];warmFeatures|=BigInt(byte&127)<<warmShift;if(!(byte&128))break;warmShift+=7n;}assert(warmFeatures&8192n);
 warmSocket.message(enc.encode('{"device_id":"dev1"}'));warmSocket.message(frame(1,Uint8Array.of(1,1,0,0,1)));
 const claimed=t.terminalConnect();assert.equal(claimed.socket,warmSocket);assert(claimed.channel);assert.equal(claimed.messages.length,2);
 // 꺼내 쓰면 바로 하나를 다시 채워 다음 탭 전환도 미리 인증된 소켓을 쓴다.
 assert.equal(sockets.length,3);const fresh=t.terminalConnect();assert.equal(fresh.socket,sockets[2]);assert.notEqual(fresh.socket,warmSocket);assert.equal(sockets.length,4);
 const [[,poll]]=[...timers];assert.equal(poll.at,2500);
 document.hidden=true;now=2500;timers.clear();poll.fn();await flush();assert.equal(socket.sent.length,4);
 sandbox.KrillMobile.activeTerminal={};document.hidden=false;const [[,next]]=[...timers];timers.clear();next.fn();await flush();assert.equal(socket.sent.length,5);
 // 폴링은 직전 etag를 since로 보내고, unchanged 응답이면 화면을 다시 그리지 않는다.
 assert.deepEqual(JSON.parse(new TextDecoder().decode(socket.sent[4].slice(7))),{action:'snapshot',since:'e1'});reply({unchanged:true,etag:'e1'});await flush();assert.equal(snapshots.length,1);
 const [[,slow]]=[...timers];assert.equal(slow.at-now,10000);
 // resume() 시 살아 있는 프리워밍 소켓은 그대로 두고, 정상 응답이 1.5초 안에 오면 재연결하지 않는다.
 t.resume();await flush();assert.equal(sockets.length,4);assert.equal(socket.sent.length,6);reply();await flush();assert.equal(snapshots.length,2);
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
