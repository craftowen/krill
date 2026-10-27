// Session screen regressions without a browser or a running app.
'use strict';
const assert=require('node:assert/strict'),{getEventListeners}=require('node:events'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
class Element extends EventTarget {
 constructor(tag,text,cls){super();this.tag=tag;this.textContent=text;this.className=cls;this.children=[];this.dataset={};this.attrs={};this.style={};this.scrollLeft=0;this.scrollTop=0;const classes=new Set((cls||'').split(/\s+/));this.classList={toggle:(name,on)=>{if(on)classes.add(name);else classes.delete(name);},remove:name=>classes.delete(name),contains:name=>classes.has(name)};}
 focus(){if(this.ownerDocument)this.ownerDocument.activeElement=this;}
 append(...nodes){for(const n of nodes){n.parentElement=this;this.children.push(n);}}
 insertBefore(node,before){if(node===before)return;node.remove();const index=before?this.children.indexOf(before):-1;node.parentElement=this;if(index<0)this.children.push(node);else this.children.splice(index,0,node);}
 prepend(...nodes){for(const n of nodes)n.parentElement=this;this.children.unshift(...nodes);}
 replaceChildren(...nodes){this.children=[];this.append(...nodes);}
 setAttribute(k,v){this.attrs[k]=v;}
 getAttribute(k){return this.attrs[k];}
 get nextSibling(){const nodes=this.parentElement?.children||[];return nodes[nodes.indexOf(this)+1]||null;}
 querySelector(selector){return this.children.flatMap(n=>[n,...n.descendants()]).find(n=>selector==='h1'?n.tag==='h1':selector==='[aria-selected=true]'&&n.attrs['aria-selected']==='true');}
 descendants(){return this.children.flatMap(n=>[n,...n.descendants()]);}
 contains(node){return node===this||this.descendants().includes(node);}
 scrollIntoView(){}
 remove(){if(this.parentElement)this.parentElement.children=this.parentElement.children.filter(n=>n!==this);}
}
const longPressCode=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8').split('\n').find(l=>l.includes('function longPress('));
function fixture(sessions=[{id:7,state:'Working',branch:null}],snapshot,tablet=false,preferences={},options={}){
 const timers=new Map(),mounts=[],buttons=[],sheets=[],calls=[],routes=[],remembered=[],created=[],abort=new AbortController(),wide=new EventTarget(),viewport=new EventTarget(),windowEvents=new EventTarget();wide.matches=tablet;let clock=0,id=0;
 let resize;const doc={activeElement:null,createElementNS:(_,tag)=>new Element(tag)};const sandbox={addEventListener:windowEvents.addEventListener.bind(windowEvents),removeEventListener:windowEvents.removeEventListener.bind(windowEvents),dispatchEvent:windowEvents.dispatchEvent.bind(windowEvents),visualViewport:viewport,ResizeObserver:class{constructor(fn){resize=fn;}observe(){}disconnect(){}},AbortController,abort,matchMedia:()=>wide,document:doc,setTimeout:(fn,ms)=>{timers.set(++id,{fn,at:clock+ms});return id;},clearTimeout:id=>timers.delete(id),KrillMobile:{terminal:{mount(root,options){const port=new Element('div',undefined,'terminal-port'),capture=new Element('textarea');capture.tagName='TEXTAREA';port.append(capture);root.append(port);const h={root,port,capture,options,focused:0,unmounted:false,fitRequests:[],fitToPhone(...args){h.fitRequests.push(args);return Promise.resolve();},setInputActive(active){h.inputActive=active;if(!active&&sandbox.document.activeElement===capture)sandbox.document.activeElement=null;},focus(){h.focused++;sandbox.document.activeElement=capture;},unmount(){h.unmounted=true;options.onStatus({phase:'disconnected',message:'Disconnected'});}};mounts.push(h);options.onStatus({phase:'connecting'});return h;}}}};
 vm.createContext(sandbox);vm.runInContext(longPressCode,sandbox);vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/session.js'),'utf8'),sandbox);
 const group={key:'g',label:'Workspace',sessions},state={groups:new Map([['g',group]]),sessions:new Map(sessions.map(s=>[s.id,s])),connection:{phase:'ready'},preferences,drafts:new Map(),snapshot};
 const el=(...args)=>{const n=new Element(...args);n.ownerDocument=doc;return n;},ctx={el,state,route:{groupKey:'g',sessionId:7},signal:abort.signal,button(label,action,cls){const b=el('button',label,cls);b.setAttribute('aria-label',label);b.action=action;buttons.push(b);return b;},header(){const h=el('header'),copy=el('div');copy.append(el('h1'));h.append(copy);return h;},icon:()=>el('svg'),dot:status=>el('span',undefined,`status-dot ${String(status).toLowerCase()}`),longPress:sandbox.longPress,groupRoute:(g,id)=>`/host/local/group/${g.key}/s/${id??g.sessions[0]?.id}`,go(r){routes.push(r);},rerender(){},savePreferences(p){Object.assign(state.preferences,p);},rememberSession:(s,g)=>remembered.push([s.id,g.key]),error(){},photo(){},transport:{},sheet:(title,entries,_,render)=>{entries.title=title;entries.render=render;sheets.push(entries);},closeSheet(){},rpc:async request=>calls.push(request),refresh:async()=>{},createSession(group,agent,request){calls.push(request);created.push([group.key,agent||'']);}};
 if(options.chat)sandbox.KrillMobile.chat={mount(){return {setVisible(){},update(){},unmount(){}};}};if(options.sessionKey)ctx.sessionKey=options.sessionKey;
 const root=el('main'),view=sandbox.KrillMobile.screens.session(root,ctx);
 function advance(ms){clock+=ms;for(const [key,t] of [...timers])if(t.at<=clock){timers.delete(key);t.fn();}}
 return {resize:width=>resize([{contentRect:{width}}]),view,state,ctx,abort,wide,root,sandbox,longPress:sandbox.longPress,mounts,timers,advance,buttons,sheets,calls,routes,remembered,created};
}
async function testDefaultSessionView(){
 const cases=[ [{},false,'term'],[{},true,'term'],[{defaultSessionView:'chat'},true,'chat'],[{defaultSessionView:'term'},true,'term'],[{defaultSessionView:'invalid'},true,'term'],[{defaultSessionView:'chat'},false,'term'],[{defaultSessionView:'chat',sessionViewModes:{'host:7':'term'}},true,'term'],[{defaultSessionView:'term',sessionViewModes:{'host:7':'chat'}},true,'chat'],[{defaultSessionView:'chat',sessionViewModes:{'host:7':'invalid'}},true,'term'],[{defaultSessionView:'chat',sessionViewModes:{'host:7':null}},true,'term'],[{defaultSessionView:'term',sessionViewModes:{'other-host:7':'chat',7:'chat'}},true,'term'] ];
 for(const [preferences,chat,expected] of cases){const f=fixture(undefined,undefined,false,preferences,{chat,sessionKey:id=>'host:'+id}),mode=f.buttons.find(b=>b.className==='ghost small mode-toggle');assert.equal(mode.textContent,expected==='chat'?'Term':'Chat',JSON.stringify(preferences));assert.equal(f.sandbox.KrillMobile.sessionView(preferences,'host:7',chat),expected);f.view.cleanup();}
 const f=fixture(undefined,undefined,false,{defaultSessionView:'chat'},{chat:true,sessionKey:id=>'host:'+id});f.buttons.find(b=>b.className==='ghost small mode-toggle').action();assert.equal(f.state.preferences.defaultSessionView,'chat');assert.equal(f.state.preferences.sessionViewModes['host:7'],'term');f.view.cleanup();
 // Restoring an old split must not target a hidden secondary session while the new default opens Chat.
 const split=fixture([{id:7,state:'Working'},{id:8,state:'Working'}],undefined,true,{defaultSessionView:'chat',tabletSplits:{tabletSplit:{session:8,active:1}}},{chat:true});assert.equal(split.mounts.length,1);assert.equal(split.mounts[0].options.sessionId,7);assert(!split.root.descendants().some(n=>n.className==='terminal-pane secondary-pane'));split.view.cleanup();
 // The existing preference writer persists the field through the existing UI storage key.
 const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8').split('\n').find(l=>l.includes('function savePreferences(')),storage=new Map(),context={state:{preferences:{}},applyPreferences(){},localStorage:{getItem:k=>storage.get(k),setItem:(k,v)=>storage.set(k,v)},error(e){throw e;}};vm.createContext(context);vm.runInContext(source+';savePreferences({defaultSessionView:"chat"});',context);assert.equal(JSON.parse(storage.get('krill-mobile-ui-v1')).defaultSessionView,'chat');
 console.log('PASS device session default storage, scoped per-session precedence, invalid values and unavailable Chat fallback to Term');
}
async function testInstalledQuickActions(){
 const f=fixture([{id:7,agent:'codex',state:'Working',cwd:'/repo'},{id:8,agent:'pi',state:'Waiting',cwd:'/repo'}],{capabilities:{command_catalog:true},quick_commands:[{name:'Existing',text:'$review '}]},true),M=f.sandbox.KrillMobile;
 const nodes=b=>b.descendants(),by=(b,label)=>nodes(b).find(n=>n.tag==='button'&&n.textContent===label),flush=async()=>{for(let i=0;i<20;i++)await Promise.resolve();},body=()=>{f.buttons.find(b=>b.className==='icon-button session-quick-actions').action();const root=f.ctx.el('div');f.sheets.at(-1).render(root);return root;};
 let release;f.ctx.rpc=async request=>{f.calls.push(request);if(request.action==='save_quick_commands')return {quick_commands:request.commands};return new Promise(resolve=>{release=resolve;});};
 let b=body();by(b,'Find installed skills').action();await flush();assert.deepEqual(f.calls.map(r=>r.session),[7]);const search=nodes(b).find(n=>n.type==='search');assert.equal(search.getAttribute('aria-label'),'Search skills and commands');assert.equal(f.sandbox.document.activeElement,search);assert(!nodes(b).some(n=>n.textContent==='Loading installed skills…'));f.advance(1000);assert(nodes(b).some(n=>n.textContent==='Loading installed skills…'));
 release({supported:true,truncated:true,items:[{name:'Review',text:'$review',description:'Existing command'},{name:'Deploy <img>',text:'$deploy',description:'Ship safely <script>'},{name:'Duplicate',text:'$deploy ',description:'same invocation'},{name:'Broken'}]});await flush();assert.equal(nodes(b).filter(n=>n.className==='quick-action-card').length,2);assert(by(b,'Already saved').disabled);assert(nodes(b).some(n=>n.textContent==='Ship safely <script>'));search.value='ship safely';search.dispatchEvent(new Event('input'));assert.equal(nodes(b).filter(n=>n.className==='quick-action-card').length,1);assert.equal(f.calls.length,1,'filtering reuses catalog');await by(b,'Add to quick actions').action();assert.equal(f.calls.at(-1).action,'save_quick_commands');assert.equal(f.state.snapshot.quick_commands[1].text,'$deploy');assert(by(b,'Already saved').disabled);assert.equal(f.calls.filter(q=>q.action==='prompt').length,0);
 search.value='missing';search.dispatchEvent(new Event('input'));assert(nodes(b).some(n=>n.textContent==='No skills or commands match your search.'));const escape=new Event('keydown',{cancelable:true});escape.key='Escape';nodes(b).find(n=>n.className==='quick-catalog').dispatchEvent(escape);assert(escape.defaultPrevented);assert(by(b,'Find installed skills'));
 // A fresh picker loads once, and returning to the saved list invalidates a pending result.
 by(b,'Find installed skills').action();await flush();by(b,'Back').action();release({supported:true,items:[{name:'Late',text:'$late'}]});await flush();assert(!nodes(b).some(n=>n.textContent==='Late'));
 // The actual active split session, rather than the route's primary session, owns the lookup.
 f.buttons.find(n=>n.textContent==='Split terminal').action();f.sheets.at(-1).find(n=>n.label.endsWith('#8')).action();assert.equal(M.activeTerminal.options.sessionId,8);b=body();by(b,'Find installed skills').action();await flush();assert.equal(f.calls.at(-1).session,8);release({supported:true,items:[{name:'Pi skill',text:'/skill:pi-review',description:'Pi only'}]});await flush();await by(b,'Add to quick actions').action();assert.equal(f.state.snapshot.quick_commands.at(-1).text,'/skill:pi-review');
 // An active-pane change makes retained catalog rows harmless.
 b=body();by(b,'Find installed skills').action();await flush();const primary=f.root.descendants().find(n=>n.className==='ghost pane-title'&&n.parentElement.parentElement.dataset.session===7);primary.action();release({supported:true,items:[{name:'Wrong pane',text:'/skill:wrong'}]});await flush();assert(!nodes(b).some(n=>n.textContent==='Wrong pane'));assert(nodes(b).some(n=>n.textContent==='Session context changed. Reopen installed skills.'));
 // A same-ID account binding/profile config change is exposed as an opaque server context.
 f.state.sessions.get(7).command_context='binding-A';b=body();by(b,'Find installed skills').action();await flush();release({supported:true,items:[{name:'Bound skill',text:'$bound'}]});await flush();const boundAction=by(b,'Add to quick actions'),boundCalls=f.calls.length,boundTarget=f.state.sessions.get(f.calls.at(-1).session);boundTarget.command_context='binding-B';f.view.update();assert(!nodes(b).some(n=>n.textContent==='Bound skill'));await boundAction.action();assert.equal(f.calls.length,boundCalls);boundTarget.command_context='binding-A';f.view.update();await boundAction.action();assert.equal(f.calls.length,boundCalls,'returning to an old opaque context cannot revive the old result');
 for(const [data,message] of [[{supported:false,items:[]},'Installed skills are unavailable for this agent.'],[{supported:true,items:[]},'No installed skills or commands found.']]){b=body();by(b,'Find installed skills').action();await flush();release(data);await flush();assert(nodes(b).some(n=>n.textContent===message));}
 f.ctx.rpc=async req=>{f.calls.push(req);throw Error('private desktop path');};b=body();by(b,'Find installed skills').action();await flush();assert(nodes(b).some(n=>n.textContent==='Could not load installed skills. Reopen to try again.'));assert(!nodes(b).some(n=>n.textContent==='private desktop path'));
 // Host scope changes hide late responses and prevent late saves from replacing another desktop's snapshot.
 let host='A';f.ctx.sessionKey=id=>host+':'+id;f.ctx.rpc=async req=>{f.calls.push(req);return new Promise(resolve=>{release=resolve;});};b=body();by(b,'Find installed skills').action();await flush();host='B';release({supported:true,items:[{name:'Other account',text:'$secret'}]});await flush();assert(!nodes(b).some(n=>n.textContent==='Other account'));delete f.state.snapshot.quick_commands;assert.equal(M.quickCommands.read(f.ctx).length,0,'legacy/global phone commands never leak into another scoped desktop');host='A';assert.equal(M.quickCommands.read(f.ctx).length,3,'own desktop saved cache survives host changes');
 f.state.snapshot.quick_commands=[{name:'Before save',text:'before'}];const saving=M.quickCommands.save(f.ctx,[{name:'Own',text:'own'}]);host='B';f.state.snapshot={quick_commands:[{name:'Other host',text:'other'}]};release({quick_commands:[{name:'Late normalized',text:'late'}]});await saving;assert.equal(f.state.snapshot.quick_commands[0].name,'Other host');
 // Route changes and session removal also fence delayed metadata.
 host='A';f.state.snapshot={capabilities:{command_catalog:true},quick_commands:[]};f.sandbox.location={hash:'#/session'};b=body();by(b,'Find installed skills').action();await flush();f.sandbox.location.hash='#/settings';release({supported:true,items:[{name:'Old route',text:'$route'}]});await flush();assert(!nodes(b).some(n=>n.textContent==='Old route'));
 f.sandbox.location.hash='#/session';b=body();by(b,'Find installed skills').action();await flush();f.state.sessions.delete(7);release({supported:true,items:[{name:'Ended session',text:'$ended'}]});await flush();assert(!nodes(b).some(n=>n.textContent==='Ended session'));
 f.view.cleanup();console.log('PASS installed quick actions: active split context, literal CLI invocation, search/cache, dedup, metadata safety, empty/unsupported/error, Escape and stale pane/host/save responses');
}
function testHostViewBudgets(){
 const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8').split('\n'),hostViews=new Map(),state={drafts:new Map(),chatCache:new Map(),titles:new Map(),ui:{}},document={selected:false,querySelector(selector){return selector.includes('.chat-opt')&&document.selected?{}:null;}},context={hostViews,state,document,root:{contains:()=>true},productPending:0,terminalBusy:false,activeView:null,hostGeneration:0,tabletList:null,tabletAbort:null,tabletContext:null,tabletShell:null,screen:{cleanup(){}},abort:null,transport:null,pool:null,pendingRecent:null,deepLink:null,parameters:new URLSearchParams(),history:{replaceState(){}},location:{pathname:'/',hash:'#/'},sessionScope:()=> 'scope',credentials:null,endpoint:{selectHost:id=>({device_id:'device-'+id})},group(){},M:{terminal:{updateBusy:()=>context.terminalBusy},recentSession:{read:()=>null}}};vm.createContext(context);vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/chat.js'),'utf8'),context);Object.assign(context.M,context.KrillMobile);const markers=['const viewKey=','const hostViewPins=','function hostViewPinned(','function hostViewWeight(','function pruneHostViews(','function rememberHostView(','function clearHost(','function activateEntry('];vm.runInContext(markers.map(marker=>source.find(line=>line.trim().startsWith(marker))).join('\n')+'\nglobalThis.H={rememberHostView,pruneHostViews,clearHost,activateEntry};',context);const H=context.H,key=(id,device='device-'+id)=>JSON.stringify([id,device]);
 function put(id,options={}){Object.assign(state,{drafts:new Map(options.draft?[[7,options.draft]]:[]),chatCache:new Map([[7,{retainedBytes:options.bytes??4*1024*1024,...options.entry}]]),titles:new Map(),ui:{search:'',filter:'all'}});document.selected=!!options.selected;context.terminalBusy=!!options.history;H.rememberHostView(id,'device-'+id);document.selected=false;context.terminalBusy=false;}
 for(let n=0;n<40;n++){put('clean-'+n);H.pruneHostViews(null);}assert.equal(hostViews.size,8,'host count bounded across visits');assert(!hostViews.has(key('clean-0')));
 hostViews.clear();put('draft',{draft:'unsent'});put('photo',{entry:{photo:{status:'ready'}}});put('sending',{entry:{sending:true}});put('model',{entry:{modelPending:{scope:'same-session'}}});put('live',{entry:{live:{items:[]}}});put('mounted',{entry:{mounts:1}});put('selection',{selected:true});put('history',{history:true});for(let n=0;n<20;n++){put('pressure-'+n);H.pruneHostViews(key('mounted'));}for(const id of ['draft','photo','sending','model','live','mounted','selection','history'])assert(hostViews.has(key(id)),id+' pinned');assert.equal(hostViews.size,8);
 hostViews.clear();for(let n=0;n<6;n++)put('heavy-'+n,{bytes:7*1024*1024,entry:{mounts:1}});for(const view of hostViews.values())view.chatCache.get(7).mounts=0;H.pruneHostViews(null);assert.equal(hostViews.size,4,'aggregate weight budget applies below host-count limit');
 hostViews.clear();put('same',{draft:'device A'});const previous=hostViews.get(key('same')),warmActivations=[];H.activateEntry({id:'same',transport:{close(){},setActive(active){warmActivations.push(active);}},connection:{phase:'ready'}});assert.deepEqual(warmActivations,[true]);assert.equal(state.drafts,previous.drafts);assert.equal(state.drafts.get(7),'device A');assert.equal(hostViews.keys().next().value,key('same'));
 context.endpoint.selectHost=()=>({device_id:'other-device'});Object.assign(state,{drafts:new Map(),chatCache:new Map(),titles:new Map(),ui:{}});H.activateEntry({id:'same',transport:{close(){}},connection:{phase:'ready'}});assert.equal(state.drafts.size,0,'device-generation key cannot restore the previous device draft');
 context.activeView={id:'final',device:'final-device'};state.drafts.set(7,'kept by clearHost');H.clearHost();assert.equal(hostViews.get(key('final','final-device')).drafts.get(7),'kept by clearHost');assert.equal(state.drafts.size,0);assert.equal(context.transport,null);
 console.log('PASS hostViews count/aggregate LRU budgets, draft/photo/sending/live/mount/selection/history pins, exact device restore and clearHost retention');
}
function testSessionBackSkipsSessionHistory(){
 const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8').split('\n'),entries=['#/host/mac'],context={pendingRecent:null,renders:0,endpoint:{profiles:()=>[{id:'mac',active:true}]},render(){context.renders++;}};let index=0;
 context.location={hash:entries[0],pathname:'/',search:''};
 context.history={replaceState(_,__,url){entries[index]=url.slice(url.indexOf('#'));},back(){index--;context.hashchange();}};
 vm.createContext(context);vm.runInContext(['let routeStack=','const hostRoute=','const sessionPath=','const go=','function backTo('].map(marker=>source.find(line=>line.trim().startsWith(marker))).join('\n')+`
 globalThis.go=go;globalThis.backTo=backTo;globalThis.stack=()=>routeStack;globalThis.hashchange=()=>{const hash=location.hash||'#/';if(historyBack){routeStack.pop();historyBack=false;routeStack[routeStack.length-1]=hash;}else if(routeStack.at(-2)===hash)routeStack.pop();else if(routeStack.at(-1)!==hash)routeStack.push(hash);render();};`,context);
 // A hash assignment pushes a browser entry and fires hashchange.
 Object.defineProperty(context.location,'hash',{configurable:true,get(){return entries[index];},set(v){if(v===entries[index])return;entries.splice(index+1,Infinity,v);index++;context.hashchange();}});
 const a='#/host/mac/group/g/s/1',b='#/host/mac/group/g/s/2',c='#/host/mac/group/h/s/3';
 context.go(a.slice(1));context.go(b.slice(1));context.go(a.slice(1));context.go(c.slice(1));
 assert.deepEqual(entries,['#/host/mac',c],'session-to-session moves replace the session entry instead of stacking');
 context.backTo('/host/local');assert.equal(entries[index],'#/host/mac','Back from a session reached from other sessions lands on the project list');assert.equal(index,0);
 context.go('/git/3');context.go(a.slice(1));assert.equal(entries.length,3,'non-session screens still push history');
 console.log('PASS session Back goes straight to the project list even after moving between sessions');
}
function testImeDefersSnapshotUpdates(){
 const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8').split('\n'),document=Object.assign(new EventTarget(),{hidden:false}),updates=[],context={document,routeKey:'#/s',location:{hash:'#/s'},screen:{update:()=>updates.push(document.value)},tabletMedia:null,state:{preferences:{}},tabletList:null};vm.createContext(context);vm.runInContext(['let imeComposing=','function update(){'].map(marker=>source.find(line=>line.trim().startsWith(marker))).join('\n')+'\nglobalThis.update=update;',context);
 const fire=type=>document.dispatchEvent(new Event(type));document.value='before';context.update();fire('compositionstart');document.value='ㅎ';context.update();document.value='하';context.update();assert.deepEqual(updates,['before'],'no screen update while an IME composition is open');fire('compositionend');assert.deepEqual(updates,['before','하'],'one catch-up update when composition ends');fire('compositionend');context.update();assert.equal(updates.length,3);
 fire('compositionstart');context.update();fire('focusout');assert.equal(updates.length,4,'blur without compositionend still releases the gate');fire('compositionstart');fire('compositionend');assert.equal(updates.length,4,'no update when nothing was missed');
 console.log('PASS IME composition defers snapshot screen updates and catches up once');
}
async function testQuickActions(){
 const f=fixture([{id:7,state:'Working'},{id:8,state:'Waiting'}],{quick_commands:[{name:'Orchestrate',text:'/krill-orchestration '},{name:'Git status',text:'git status'}]},true),seen=[];
 f.ctx.rpc=async request=>{f.calls.push(request);return {quick_commands:request.commands};};
 const wire=h=>h.insertText=text=>{assert(!text.includes('\r'));seen.push([h.options.sessionId,text]);return true;};
 const first=f.mounts[0];wire(first);
 const quick=f.buttons.find(b=>b.className==='icon-button session-quick-actions');assert.equal(quick.parentElement.tag,'header');assert.equal(quick.attrs['aria-haspopup'],'dialog');
 const open=h=>{if(f.mounts.length>1){const title=f.root.descendants().find(n=>n.className==='ghost pane-title'&&n.parentElement.parentElement.contains(h.root));title?.action();}quick.action();const body=new Element('div');f.sheets.at(-1).render(body);return body;},all=body=>body.descendants(),by=(body,label)=>all(body).find(n=>n.textContent===label&&n.tag==='button');
 let body=open(first);assert.equal(f.sheets.at(-1).title,'Quick actions');assert.equal(f.sheets.at(-1).length,0,'quick actions contain saved commands only');let cards=all(body).filter(n=>n.className==='quick-action-insert');assert.equal(cards[0].children[0].textContent,'Orchestrate');cards[0].action();assert.deepEqual(seen,[[7,'/krill-orchestration ']]);
 f.buttons.find(b=>b.textContent==='Split terminal').action();f.sheets.at(-1)[0].action();const second=f.mounts.at(-1);wire(second);
 // A quick-action activation also selects its pane when no pointer event ran first.
 body=open(first);all(body).find(n=>n.className==='quick-action-insert').action();body=open(second);all(body).find(n=>n.className==='quick-action-insert').action();assert.deepEqual(seen.slice(-2),[[7,'/krill-orchestration '],[8,'/krill-orchestration ']]);
 body=open(second);by(body,'+ Add').action();let fields=all(body),name=fields.find(n=>n.tag==='input'),text=fields.find(n=>n.tag==='textarea');name.value='Ship';text.value='/ship-dev ';await by(body,'Save').action();assert.equal(f.calls.at(-1).action,'save_quick_commands');assert.equal(f.state.preferences.quickCommands.at(-1).text,'/ship-dev ');assert.equal(f.state.snapshot.quick_commands.length,3);
 by(body,'Edit').action();fields=all(body);fields.find(n=>n.tag==='input').value='Prompt';fields.find(n=>n.tag==='textarea').value='Review this\nthen explain';await by(body,'Save').action();assert.equal(f.state.preferences.quickCommands[0].name,'Prompt');assert.equal(f.calls.at(-1).commands[0].text,'Review this\nthen explain');
 await by(body,'Delete').action({stopPropagation(){}});assert.equal(f.state.preferences.quickCommands.length,2);assert.equal(f.calls.at(-1).commands.length,2);
 let errors=[];f.ctx.error=e=>errors.push(e);f.ctx.rpc=async()=>{throw Error('offline');};by(body,'+ Add').action();fields=all(body);fields.find(n=>n.tag==='input').value='Offline';fields.find(n=>n.tag==='textarea').value='kept locally';await by(body,'Save').action();assert.equal(errors.length,1);assert.equal(f.state.preferences.quickCommands.at(-1).name,'Offline');assert(!f.calls.some(q=>q.action==='prompt'));
 f.view.cleanup();
 // REMOTE-005: delayed save_quick_commands completion or failure from Host A after abort / switch to Host B must never clobber Host B or show a toast on Host B.
 {
  const plain=v=>JSON.parse(JSON.stringify(v));
  const race=fixture([{id:7,state:'Working'}],{host:{display_name:'Host A'},quick_commands:[]},false),Q=race.sandbox.KrillMobile.quickCommands;
  let activeHost='host-a',resolveA,rejectA2;const toastErrors=[];
  race.ctx.sessionKey=id=>`${activeHost}\n${id}`;race.ctx.error=e=>toastErrors.push(e);
  const cmdA=[{name:'Host A',text:'echo a'}],cmdB=[{name:'Host B',text:'echo b'}];
  race.ctx.rpc=()=>new Promise(r=>{resolveA=r;});
  const pendingResolve=Q.save(race.ctx,cmdA);
  race.abort.abort();activeHost='host-b';race.state.snapshot={host:{display_name:'Host B'},quick_commands:cmdB};Q.read(race.ctx);
  resolveA({quick_commands:[{name:'Host A normalized',text:'echo a'}]});await pendingResolve;
  assert.deepEqual(plain(race.state.snapshot.quick_commands),cmdB,'aborted Host A save cannot overwrite Host B snapshot');
  assert.deepEqual(plain(race.state.preferences.quickCommands),cmdB,'aborted Host A save cannot overwrite active quickCommands');
  assert.deepEqual(plain(race.state.preferences.quickCommandsByHost['host-b\nquickCommands']),cmdB);
  const liveAbort=new AbortController();race.ctx.signal=liveAbort.signal;activeHost='host-a';race.state.snapshot={host:{display_name:'Host A'},quick_commands:cmdA};
  race.ctx.rpc=()=>new Promise((_,rej)=>{rejectA2=rej;});
  const pendingReject=Q.save(race.ctx,cmdA);
  activeHost='host-b';race.state.snapshot={host:{display_name:'Host B'},quick_commands:cmdB};
  rejectA2(Error('network down'));await pendingReject;
  assert.equal(toastErrors.length,0,'stale Host A failure cannot surface error toast on Host B');
  assert.deepEqual(plain(race.state.snapshot.quick_commands),cmdB);
  // Offline Host C with no snapshot quick_commands does not inherit Host A/B scoped cache.
  activeHost='host-c';race.state.snapshot={host:{display_name:'Host C'}};
  assert.deepEqual(plain(Q.read(race.ctx)),[],'unvisited offline host does not inherit another host quickCommands');
  // Same-host concurrent snapshot refresh still receives authoritative saved quick_commands.
  activeHost='host-a';race.state.snapshot={host:{display_name:'Host A'},quick_commands:[]};
  let resolveSame;race.ctx.rpc=()=>new Promise(r=>{resolveSame=r;});
  const pendingSame=Q.save(race.ctx,cmdA);
  race.state.snapshot={host:{display_name:'Host A'},quick_commands:cmdA};
  resolveSame({quick_commands:[{name:'Saved A',text:'echo a'}]});await pendingSame;
  assert.deepEqual(plain(race.state.snapshot.quick_commands),[{name:'Saved A',text:'echo a'}]);
  race.view.cleanup();
 }
 const fallback=fixture(undefined,{},false,{quickCommands:[{name:'Fallback',text:'saved prompt'}]});const fallbackBody=openFallback();function openFallback(){fallback.buttons.find(b=>b.className==='icon-button session-quick-actions').action();const b=new Element('div');fallback.sheets.at(-1).render(b);return b;}assert(all(fallbackBody).some(n=>n.textContent==='Fallback'));fallback.view.cleanup();
 console.log('PASS saved quick actions snapshot/fallback, active split insertion without Enter, add/edit/delete RPC and preferences, offline local retention');
}
function testSplitContinuity(){
 for(const direction of ['auto','columns','rows'])for(const active of [0,1]){
  const f=fixture([{id:7,state:'Working'},{id:8,state:'Waiting'}],undefined,true),M=f.sandbox.KrillMobile;
  f.buttons.find(b=>b.textContent==='Split terminal').action();f.sheets.at(-1)[0].action();
  const panels=f.root.descendants().find(n=>n.className==='session-panels'),panes=panels.children,control=f.buttons.find(b=>b.className==='ghost split-direction');
  control.action();const choices=new Element('div');f.sheets.at(-1).render(choices);choices.descendants().find(n=>n.dataset.direction===direction).action();
  panes[active].dispatchEvent(new Event('pointerdown'));const handles=[...f.mounts];handles[active].focus();
  handles.forEach((h,i)=>{h.port.scrollTop=120+i*80;h.port.scrollLeft=15+i*10;});
  const tabScroll=f.root.descendants().find(n=>n.className==='tab-scroll');tabScroll.scrollLeft=73;
  const saved=JSON.stringify(f.state.preferences.tabletSplits.tabletSplit),focused=f.sandbox.document.activeElement;
  for(const [width,wide] of [[1100,true],[390,false],[720,false],[799,false],[800,false],[1200,true]]){
   f.wide.matches=wide;f.wide.dispatchEvent(new Event('change'));f.resize(width);
   f.sandbox.dispatchEvent(new Event('orientationchange'));f.sandbox.dispatchEvent(new Event('resize'));f.sandbox.visualViewport.dispatchEvent(new Event('resize'));f.view.update();
   assert.equal(f.mounts.length,2,`${direction}/${active}: resize retains both connections`);
   assert(handles.every(h=>!h.unmounted));assert.equal(M.activeTerminal,handles[active]);assert.equal(f.sandbox.document.activeElement,focused);
   assert.equal(panels.classList.contains('is-split'),true);assert(panes.every(p=>p.hidden===false));assert.equal(control.parentElement.hidden,false);
   assert.equal(panels.dataset.layout,direction==='rows'||width<(direction==='columns'?721:800)?'rows':'columns');
   handles.forEach((h,i)=>{assert.equal(h.root.children[0],h.port);assert.equal(h.port.scrollTop,120+i*80);assert.equal(h.port.scrollLeft,15+i*10);assert.equal(h.inputActive,i===active);});
   assert.equal(tabScroll.scrollLeft,73);assert.equal(JSON.stringify(f.state.preferences.tabletSplits.tabletSplit),saved);assert.equal(f.timers.size,0);assert.equal(f.routes.length,0);assert.equal(f.calls.length,0);
  }
  f.view.cleanup();assert(handles.every(h=>h.unmounted));assert.equal(getEventListeners(f.wide,'change').length,0);
 }
 // A split restored while narrow retains its chosen pane and both mounts too.
 const f=fixture([{id:7,state:'Working'},{id:8,state:'Working'}],undefined,false,{tabletSplits:{tabletSplit:{session:8,active:1,direction:'rows'}}});
 assert.equal(f.mounts.length,2);assert.equal(f.sandbox.KrillMobile.activeTerminal.options.sessionId,8);f.view.cleanup();
 console.log('PASS auto/columns/rows keep both mounts, focused input, pane selection and scroll across rotation, keyboard and fold widths');
}
function testSplitTabReplacement(){
 for(const active of [0,1]){
  const f=fixture([{id:7,state:'Working'},{id:8,state:'Waiting'},{id:9,state:'Done'}],undefined,true),M=f.sandbox.KrillMobile;
  f.ctx.sessionKey=id=>'host:'+id; // Replacement preferences use the selected session's scoped key.
  f.buttons.find(b=>b.textContent==='Split terminal').action();f.sheets.at(-1)[0].action();
  const panels=f.root.descendants().find(n=>n.className==='session-panels'),panes=panels.children,handles=[...f.mounts],inactive=handles[1-active],old=handles[active],tabs=()=>f.root.descendants().filter(n=>n.className==='session-tab');
  panes[active].dispatchEvent(new Event('pointerdown'));inactive.port.scrollTop=456;inactive.port.scrollLeft=35;old.options.onStatus({phase:'disconnected'});
  tabs().find(n=>n.dataset.session===9).action();const replacement=f.mounts.at(-1);
  assert.equal(f.mounts.length,3);assert.equal(replacement.options.sessionId,9);assert.equal(replacement.options.stateKey,'host:9');assert(old.unmounted);assert(!inactive.unmounted);
  assert.equal(M.activeTerminal,replacement);assert.equal(replacement.inputActive,true);assert.equal(inactive.inputActive,false);assert.equal(panes[active].dataset.session,9);assert.equal(panes[1-active].dataset.session,inactive.options.sessionId);
  assert.equal(inactive.root.children[0],inactive.port);assert.equal(inactive.port.scrollTop,456);assert.equal(inactive.port.scrollLeft,35);assert.equal(tabs().find(n=>n.dataset.session===9).attrs['aria-selected'],'true');assert.equal(tabs().find(n=>n.dataset.session===inactive.options.sessionId).disabled,true);
  const inputChange=replacement.options.onPreferenceChange;inputChange({inputMode:'buffered'});assert.equal(f.state.preferences.terminalInputModes['host:9'],'buffered');
  old.options.onTitle('stale title');old.options.onStatus({phase:'disconnected'});assert(!f.state.titles.has(9));f.advance(65000);f.view.update();assert.equal(f.mounts.length,3);assert.equal(f.timers.size,0);assert.equal(f.routes.length,0);assert.equal(f.calls.length,0);
  // The displaced session can be selected again without disturbing the other pane.
  tabs().find(n=>n.dataset.session===old.options.sessionId).action();assert(replacement.unmounted);assert.equal(M.activeTerminal.options.sessionId,old.options.sessionId);assert(!inactive.unmounted);assert.equal(f.routes.length,0);f.view.cleanup();
 }
 console.log('PASS third tab replaces either active pane in place, cancels stale retries and preserves the inactive terminal and scroll');
}
async function main(){
 await testQuickActions();await testDefaultSessionView();await testInstalledQuickActions();testSplitContinuity();testSplitTabReplacement();
 testHostViewBudgets();testImeDefersSnapshotUpdates();testSessionBackSkipsSessionHistory();
 {const f=fixture([{id:7,state:'Working'},{id:8,state:'Done'},{id:9,state:'Exited'},{id:10,state:'Working',exited:true}]);
  assert.deepEqual(f.root.descendants().filter(n=>n.className==='session-tab').map(n=>n.dataset.session),[7,8]);
  f.mounts[0].options.onStatus({phase:'disconnected'});assert.equal(f.timers.size,1);
  f.state.sessions.get(7).exited=true;f.view.update();assert.equal(f.routes.at(-1),'/host/local/group/g/s/8');assert.equal(f.timers.size,0);f.advance(60000);assert.equal(f.mounts.length,1);f.view.cleanup();
 }
 {const f=fixture();f.state.sessions.delete(7);f.view.update();assert.equal(f.routes.at(-1),'/host/local','missing selected session leaves the terminal screen');f.view.cleanup();}
 {const f=fixture();f.state.groups.clear();f.state.sessions.clear();f.view.update();assert.equal(f.routes.at(-1),'/host/local','removed workspace leaves without updating the old summary');f.view.cleanup();}
 {const f=fixture();f.state.sessions.get(7).state='Exited';f.view.update();assert.equal(f.routes.at(-1),'/host/local');f.view.cleanup();}
 {const f=fixture([{id:7,state:'Exited'},{id:8,state:'Working'}]);assert.equal(f.mounts.length,0);assert.equal(f.routes.length,0,'initial redirect waits until the screen has mounted');await Promise.resolve();assert.equal(f.routes.at(-1),'/host/local/group/g/s/8');}
 {const f=fixture([{id:7,state:'Exited'}]);f.abort.abort();await Promise.resolve();assert.equal(f.routes.length,0,'abandoned initial redirects cannot change the route');}
 {const f=fixture([{id:7,state:'Working'},{id:8,state:'Working'}]);f.mounts[0].options.onStatus({phase:'ended'});assert.equal(f.routes.length,0,'exit navigation waits until snapshot pruning finishes');await Promise.resolve();assert.equal(f.routes.at(-1),'/host/local/group/g/s/8');assert.equal(f.calls.length,0,'exit cleanup never terminates a PTY');f.view.cleanup();}
 {const f=fixture();f.mounts[0].options.onStatus({phase:'ended'});f.view.cleanup();await Promise.resolve();assert.equal(f.routes.length,0,'stale exit callbacks cannot navigate after cleanup');}
 {const f=fixture([{id:7,state:'Working'},{id:8,state:'Working'}],undefined,true);f.buttons.find(b=>b.textContent==='Split terminal').action();f.sheets.at(-1)[0].action();const secondary=f.mounts[1];secondary.options.onStatus({phase:'ended'});await Promise.resolve();assert(secondary.unmounted);assert(!f.mounts[0].unmounted);assert.equal(f.state.preferences.tabletSplits.tabletSplit,null);assert.equal(f.calls.length,0);f.view.cleanup();}
 console.log('PASS exited tabs disappear; selected/missing/stream-ended sessions leave for a live tab or workspaces; split exits preserve the other PTY');

 {const sessions=Array.from({length:64},(_,n)=>({id:n+7,agent:'codex',state:'Working'})),f=fixture(sessions);const initial=f.buttons.filter(b=>b.className==='session-tab'),marks=initial.map(b=>b.children[1]);for(let i=0;i<100;i++)f.view.update();assert.equal(f.buttons.filter(b=>b.className==='session-tab').length,64);assert(initial.every((tab,i)=>tab.children[1]===marks[i]));const removed=sessions[63],down=new Event('pointerdown');down.button=0;initial[63].dispatchEvent(down);assert.equal(f.timers.size,1);f.state.groups.get('g').sessions=sessions.slice(0,63);f.state.sessions.delete(removed.id);f.view.update();assert.equal(f.timers.size,0,'removed tab cancels pending long press');f.view.cleanup();console.log('PASS 64 keyed tabs x100 updates: new buttons/icons 0, removed-tab handlers cleaned');}

 {const f=fixture([{id:7,state:'Working'},{id:8,state:'Working'}],undefined,true),M=f.sandbox.KrillMobile;
  const width=value=>{f.wide.matches=value;f.wide.dispatchEvent(new Event('change'));};
  const first=f.mounts[0];first.options.onStatus({phase:'ready'});
  f.buttons.find(b=>b.textContent==='Split terminal').action();assert.equal(f.sheets.at(-1).length,1);assert(f.sheets.at(-1)[0].label.endsWith('#8'));f.sheets.at(-1)[0].action();
  const second=f.mounts[1];assert.equal(M.activeTerminal,second);assert.equal(second.options.preferences.fitToPhone,true);assert.equal(first.inputActive,false);assert.equal(second.inputActive,true);
  width(false);assert(!first.unmounted);assert(!second.unmounted);assert.equal(f.timers.size,0);assert.equal(M.activeTerminal,second);
  width(true);const restored=f.mounts[0];assert.equal(f.mounts.length,2);assert.equal(restored.options.sessionId,7);assert.equal(M.activeTerminal,second);
  f.root.descendants().find(n=>n.className==='terminal-pane primary-pane').dispatchEvent(new Event('pointerdown'));
  width(false);assert(!restored.unmounted);assert(!second.unmounted);assert.equal(M.activeTerminal,restored);
  width(true);const next=f.mounts.at(-1);next.options.onStatus({phase:'disconnected'});assert.equal(f.timers.size,1);
  f.state.connection.phase='disconnected';f.view.update();f.advance(1000);const count=f.mounts.length;assert(next.unmounted);assert.equal(f.timers.size,0);
  f.state.connection.phase='ready';f.view.update();assert.equal(f.mounts.length,count+1);assert.equal(f.mounts.at(-1).options.sessionId,8);
  f.root.descendants().find(n=>n.className==='terminal-pane secondary-pane').descendants().find(n=>n.textContent==='Close pane').action();assert(f.mounts.at(-1).unmounted);assert(!restored.unmounted);assert.equal(f.calls.length,0);assert.equal(f.state.preferences.tabletSplits.tabletSplit,null);f.view.cleanup();
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
 {const f=fixture([{id:7,state:'Working'},{id:8,state:'Waiting'},{id:9,state:'Done'}],undefined,true);
  f.buttons.find(b=>b.textContent==='Split terminal').action();f.sheets.at(-1)[0].action();
  const pane=cls=>f.root.descendants().find(n=>n.className==='terminal-pane '+cls),tabs=()=>f.root.descendants().filter(n=>n.className==='session-tab');
  assert.equal(pane('primary-pane').descendants().filter(n=>n.className==='pane-header').length,1);assert.equal(pane('secondary-pane').descendants().filter(n=>n.className==='pane-header').length,1);
  assert.equal(pane('secondary-pane').descendants().find(n=>n.className==='ghost pane-title').attrs['aria-pressed'],'true');
  assert.equal(tabs().find(n=>n.dataset.session===7).disabled,true);tabs().find(n=>n.dataset.session===9).action();
  assert.equal(f.mounts.at(-1).options.sessionId,9);assert.equal(pane('secondary-pane').dataset.session,9);assert.equal(f.routes.length,0);
  pane('primary-pane').descendants().find(n=>n.className==='ghost pane-title').action();assert.equal(f.sandbox.KrillMobile.activeTerminal.options.sessionId,7);
  assert.equal(tabs().find(n=>n.dataset.session===9).disabled,true);tabs().find(n=>n.dataset.session===8).action();assert.equal(f.routes.length,0);assert.equal(pane('primary-pane').dataset.session,8);
  pane('primary-pane').descendants().find(n=>n.textContent==='Close pane').action();assert.equal(f.calls.length,0);assert.equal(f.state.preferences.tabletSplits.tabletSplit,null);assert.equal(f.routes.length,1);f.view.cleanup();
 }
 {const f=fixture([{id:7,state:'Working'},{id:8,state:'Working'}],undefined,true);f.sandbox.KrillMobile.chat={mount(){return {};}};
  f.buttons.find(b=>b.textContent==='Split terminal').action();f.sheets.at(-1)[0].action();f.buttons.find(b=>b.className==='ghost small mode-toggle').action();
  assert.equal(f.state.preferences.sessionViewModes[8],'chat');assert.equal(f.state.preferences.tabletSplits.tabletSplit,null);assert.equal(f.routes.length,1);assert.equal(f.calls.length,0);f.view.cleanup();
 }
 {const f=fixture([{id:7,state:'Working'},{id:8,state:'Working'}],undefined,true),primary=f.mounts[0];
  primary.options.onStatus({phase:'ready'});f.buttons.find(b=>b.textContent==='Split terminal').action();f.sheets.at(-1)[0].action();
  const requests=primary.fitRequests.length;assert.equal(requests,1,'entering a split fits the existing primary once');
  primary.options.onPreferenceChange({fitToPhone:false});for(let i=0;i<5;i++)f.view.update();
  assert.equal(primary.fitRequests.length,requests,'snapshot updates must not undo the PC mode selected in the dock');
  const secondary=f.mounts.at(-1),mountCount=f.mounts.length;
  f.wide.matches=false;f.wide.dispatchEvent(new Event('change'));f.wide.matches=true;f.wide.dispatchEvent(new Event('change'));
  assert.equal(f.mounts.length,mountCount,'folding retains both connections');
  secondary.options.onStatus({phase:'disconnected'});f.advance(1000);
  assert.equal(f.mounts.at(-1).options.preferences.fitToPhone,false,'secondary reconnect keeps the saved PC mode');
  assert.equal(primary.fitRequests.length,requests,'unfolding respects the explicit PC mode');
  primary.options.onStatus({phase:'disconnected'});f.advance(1000);
  assert.equal(f.mounts.at(-1).options.preferences.fitToPhone,false,'primary reconnect keeps the saved PC mode');f.view.cleanup();
 }
 console.log('PASS split snapshots, folding and reconnect preserve the PC mode selected in the dock');
 console.log('PASS symmetric pane headers, active labels and tab replacement, duplicate PTY disabled and primary close preserves PTY');
 console.log('PASS split direction selection, current choice, persisted state, automatic threshold and minimum pane width');
 console.log('PASS tablet split excludes duplicate PTY, preserves active pane on folding, reconnects after main transport and closes only the view');
 let f=fixture();let reconnected=0;f.ctx.transport.reconnect=()=>reconnected++;const summaryNode=f.root.descendants().find(n=>n.className==='session-summary');assert.equal(summaryNode.dataset.reconnectable,'true');summaryNode.dispatchEvent(new Event('click'));assert.equal(reconnected,1,'tapping summary while disconnected triggers transport.reconnect');assert.deepEqual(f.remembered.at(-1),[7,'g']);assert.equal(f.mounts.length,1);f.mounts[0].options.onStatus({phase:'ready'});assert.equal(summaryNode.dataset.reconnectable,undefined);summaryNode.dispatchEvent(new Event('click'));assert.equal(reconnected,1,'tapping summary while connected does not reconnect');f.mounts[0].options.onStatus({phase:'disconnected'});assert.equal(summaryNode.dataset.reconnectable,'true');for(let n=0;n<10;n++)f.view.update();assert.equal(f.timers.size,1);f.advance(999);assert.equal(f.mounts.length,1);f.advance(1);assert.equal(f.mounts.length,2);f.mounts[1].options.onStatus({phase:'disconnected'});f.advance(1999);assert.equal(f.mounts.length,2);f.advance(1);assert.equal(f.mounts.length,3);f.mounts[2].options.onStatus({phase:'disconnected'});f.view.cleanup();f.advance(60000);assert.equal(f.mounts.length,3);f.mounts[0].options.onStatus({phase:'disconnected'});assert.equal(f.timers.size,0);
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
 await body.children[0].action();assert.equal(JSON.stringify(f.calls.at(-1)),'{"action":"create_terminal","cwd_session":7,"agent":"claude"}');assert.deepEqual(f.created.at(-1),['g','claude']);
 await add.action();body=new Element('div');f.sheets.at(-1).render(body);await body.children[2].action();assert.equal(JSON.stringify(f.calls.at(-1)),'{"action":"create_terminal","cwd_session":7}');assert.deepEqual(f.created.at(-1),['g','']);
 f.buttons.find(b=>b.textContent==='Terminal actions').action();assert(!f.sheets.at(-1).some(e=>/^New .* session$/.test(e.label)));f.view.cleanup();
 console.log('PASS + opens provider sheet (old-daemon Claude/Codex fallback + Terminal) and hands the picked agent to the shared instant create flow');
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
 f.view.update();assert.equal(f.timers.size,1,'keyed redraw preserves an ongoing long press');f.advance(500);assert.equal(f.sheets.length,1);assert.equal(f.buttons.filter(b=>b.className==='session-tab').length,1,'100 unchanged updates allocate no new tab buttons');
 let stop;for(let i=0;i<100;i++){stop?.();stop=f.longPress(new Element('button'),()=>{});assert.equal(getEventListeners(f.abort.signal,'abort').length,1);}
 stop();assert.equal(getEventListeners(f.abort.signal,'abort').length,0);f.view.cleanup();
 console.log('PASS longPress abort listener cleanup across 100 redraws');
 f=fixture([{id:7,agent:'claude',state:'Waiting'},{id:8,agent:'claude',state:'Working'},{id:9,agent:null,state:'Unknown'},{id:10,agent:'antigravity',state:'Done'},{id:11,agent:'my-bot',state:'Working'},{id:12,state:'Working',exited:true},{id:13,state:'Exited'}]);
 const tabs=()=>f.buttons.filter(b=>b.className==='session-tab').slice(-5),summary=()=>tabs().map(t=>[t.children[0].className,t.children[1].attrs.class||t.children[1].className,t.children[2].textContent,t.attrs['aria-label']]);
 assert.deepEqual(summary(),[['status-dot waiting','agent-icon claude','Claude Code','Claude Code · Waiting · Terminal #7'],['status-dot working','agent-icon claude','Claude Code 2','Claude Code 2 · Working · Terminal #8'],['status-dot unknown','agent-icon','Shell','Shell · Unknown · Terminal #9'],['status-dot done','agent-icon agy','Antigravity','Antigravity · Done · Terminal #10'],['status-dot working','agent-icon custom','my-bot','my-bot · Working · Terminal #11']]);
 assert.equal(tabs()[0].children[1].children.length,1);assert.equal(tabs()[3].children[1].children.at(-1).attrs.class,'layer');assert.equal(tabs()[4].children[1].textContent,'M');
 assert.deepEqual(tabs().map(t=>t.tabIndex),[0,-1,-1,-1,-1]);
 const strip=tabs()[0].parentElement;tabs()[0].focus();
 const key=k=>{const e=new Event('keydown',{cancelable:true});e.key=k;strip.dispatchEvent(e);return e;};
 assert(key('ArrowRight').defaultPrevented);assert.equal(f.sandbox.document.activeElement,tabs()[1]);assert.deepEqual(tabs().map(t=>t.tabIndex),[-1,0,-1,-1,-1]);
 assert(key('End').defaultPrevented);assert.equal(f.sandbox.document.activeElement,tabs()[4]);assert.deepEqual(tabs().map(t=>t.tabIndex),[-1,-1,-1,-1,0]);
 assert(key('ArrowRight').defaultPrevented);assert.equal(f.sandbox.document.activeElement,tabs()[0]);assert.deepEqual(tabs().map(t=>t.tabIndex),[0,-1,-1,-1,-1]);
 assert(key('ArrowLeft').defaultPrevented);assert.equal(f.sandbox.document.activeElement,tabs()[4]);
 assert(key('Home').defaultPrevented);assert.equal(f.sandbox.document.activeElement,tabs()[0]);
 strip.scrollLeft=300;f.state.sessions.get(8).state='Waiting';f.view.update();assert.equal(tabs()[1].children[0].className,'status-dot waiting');assert.equal(tabs()[0].parentElement.scrollLeft,300);
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
 await testInstantCreate();
 await require('./mobile_quick_commands.cjs').main(fixture);
 console.log('PASS Hybrid Chat visibility, chat_poll cards, historical answers remain notes until Terminal review, and current diff modal');
}
// 새 세션은 누르는 즉시 /s/new로 옮기고, 응답이 오면 그 자리를 새 세션으로 바꾼다. 스냅샷을 기다리지 않고, 아직 스냅샷에 없는 새 세션 때문에 다른 세션으로 튀지 않는다.
async function testInstantCreate(){
 const anchored=fixture([{id:7,title:'First',state:'Working'},{id:8,title:'Second',state:'Working'}]),tabStrip=anchored.root.descendants().find(n=>n.className==='tab-scroll');tabStrip.clientWidth=100;tabStrip.scrollLeft=120;tabStrip.getBoundingClientRect=()=>({left:0,right:100});for(const [i,tab] of tabStrip.children.entries())tab.getBoundingClientRect=()=>{const grow=tabStrip.children[0].children.at(-1).textContent==='A much longer first title'?40:0;const left=(i?120+grow:0)-tabStrip.scrollLeft;return {left,right:left+120};};const visible=tabStrip.children[1],oldX=visible.getBoundingClientRect().left;anchored.state.sessions.get(7).title='A much longer first title';anchored.view.update();assert.equal(visible.getBoundingClientRect().left,oldX);assert.equal(tabStrip.scrollLeft,160);anchored.view.cleanup();
 Element.prototype.clientWidth=1200;
 const wide=fixture([{id:7,state:'Working'},{id:8,state:'Working'}],undefined,true,{tabletSplits:{tabletSplit:{session:8,active:0,direction:'auto'}}});
 assert.equal(wide.root.descendants().find(n=>n.className==='session-panels').dataset.layout,'columns','restored wide split starts in columns before ResizeObserver fires');wide.view.cleanup();delete Element.prototype.clientWidth;

 const lines=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8').split('\n'),pick=marker=>{const i=lines.findIndex(l=>l.trim().startsWith(marker));assert(i>=0,marker);return lines[i];};
 const create=lines.findIndex(l=>l.trim().startsWith('function createSession('));
 const source=[pick('function parseRoute('),pick('const groupRoute='),pick('function group(data)'),pick('let creating=null;'),lines[create],lines[create+1]].join('\n')+'\nglobalThis.A={createSession,group,current:()=>creating};';
 const routes=[],errors=[],backs=[],updates=[],refreshes=[],prewarms=[];let reply;
 const location={hash:'#/host/local/group/g/s/7'},sandbox={location,M:{},hostGeneration:0,state:{expected:new Map()},hostRoute:()=>'/host/local',update:()=>updates.push(location.hash),error:e=>errors.push(e.message),back:fallback=>{backs.push(fallback);location.hash='#'+fallback;},
  go:(path,replace=false)=>{routes.push([path,replace]);location.hash='#'+path;},transport:{rpc:request=>new Promise((resolve,reject)=>{reply={request,resolve,reject};}),refresh:()=>{refreshes.push(1);return Promise.resolve();},prewarmTerminal:()=>prewarms.push(1)}};
 vm.createContext(sandbox);vm.runInContext(source,sandbox);const A=sandbox.A,flush=async()=>{for(let i=0;i<20;i++)await Promise.resolve();};
 const snapshot={workspaces:[{key:'g',label:'Workspace',path:'/repo'}],sessions:[{id:7,worktree_key:'g',cwd:'/repo',branch:'main'}]};A.group(snapshot);
 A.createSession(sandbox.state.groups.get('g'),'claude',{action:'create_terminal',cwd_session:7,agent:'claude'});
 assert.deepEqual(routes.at(-1),['/host/local/group/g/s/new',false]);assert.equal(A.current().agent,'claude');assert.equal(reply.request.agent,'claude');assert.equal(prewarms.length,1,'the terminal socket opens alongside the create request');
 reply.resolve({session:42});await flush();
 assert.deepEqual(routes.at(-1),['/host/local/group/g/s/42',true],'the pending route is replaced in place, not pushed');assert.equal(A.current(),null);assert.equal(refreshes.length,1,'the snapshot refresh runs after navigation');
 const fresh=sandbox.state.sessions.get(42);assert.equal(fresh.agent,'claude');assert.equal(fresh.groupKey,'g');assert.equal(fresh.branch,'main');assert.deepEqual([...sandbox.state.groups.get('g').sessions.map(s=>s.id)],[7,42]);
 A.group(snapshot);assert(sandbox.state.sessions.has(42),'a snapshot without the new session keeps it');
 A.group({...snapshot,sessions:[...snapshot.sessions,{id:42,worktree_key:'g',cwd:'/repo',agent:'claude',state:'Working'}]});assert.equal(sandbox.state.sessions.get(42).state,'Working');assert.equal(sandbox.state.expected.size,0);
 // 응답 전에 다른 화면으로 가면 그 화면을 빼앗지 않는다.
 A.createSession(sandbox.state.groups.get('g'),'',{action:'create_terminal',cwd_session:7});location.hash='#/host/local';reply.resolve({session:43});await flush();assert.deepEqual(routes.at(-1),['/host/local/group/g/s/new',false]);assert.equal(updates.at(-1),'#/host/local');assert(sandbox.state.sessions.has(43));
 // 실패하면 이유를 보이고 이전 화면으로 돌아간다.
 A.createSession(sandbox.state.groups.get('g'),'codex',{action:'create_terminal',cwd_session:7,agent:'codex'});reply.reject(Error('Unknown agent'));await flush();assert.deepEqual(errors,['Unknown agent']);assert.equal(backs.at(-1),'/host/local/group/g/s/7');assert.equal(A.current(),null);
 console.log('PASS new session: instant pending route, in-place replace without waiting for the snapshot, no jump to another session, failure returns');
 // /s/new 화면: 선택된 새 탭과 빈 터미널 면을 바로 그리고, 생성이 끝나면(작업 없음) 작업 공간으로 돌린다.
 const f=fixture();f.view.cleanup();const job={group:f.state.groups.get('g'),agent:'codex'};let current=job;f.ctx.creating=()=>current;f.ctx.route={groupKey:'g',sessionId:'new'};f.ctx.groupRoute=(g,id)=>`/g/${g?.key}/${id??''}`;const root=new Element('main'),view=f.sandbox.KrillMobile.screens.session(root,f.ctx);
 const tabs=root.descendants().filter(n=>n.className==='session-tab');assert.deepEqual(tabs.map(n=>[n.attrs['aria-label'],n.attrs['aria-selected']]),[['Shell','false'],['Codex','true']]);const again=new Element('main');f.sandbox.KrillMobile.screens.session(again,{...f.ctx,creating:()=>({...job,agent:''})}).cleanup();assert.deepEqual(again.descendants().filter(n=>n.className==='session-tab').map(n=>n.attrs['aria-label']),['Shell','Shell 2'],'pending tab uses the real tab numbering');assert(root.descendants().some(n=>n.className==='terminal-mount'));const dock=root.descendants().find(n=>n.className==='command-dock');assert(dock,'pending session reserves the dock before mounting a Terminal');assert.equal(dock.attrs['aria-hidden'],'true');assert(!root.descendants().some(n=>n.className==='terminal-canvas'));assert.equal(f.mounts.length,1,'no terminal attaches before the session exists');
 const summary=root.descendants().find(n=>n.className==='session-summary').children[1];assert.equal(root.descendants().find(n=>n.className==='live-dot').dataset.live,'false');assert.equal(summary.textContent,'\u00a0','the summary line keeps its height without wording');f.advance(1000);assert.equal(summary.textContent,'Connecting…');
 current=null;view.update();assert.equal(f.routes.at(-1),'/g/g/');view.cleanup();
 console.log('PASS pending session screen: selected new tab and blank terminal surface at once, wording only after 1s');
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
 const ctx={el,state,route:{groupKey:'g',sessionId:7},signal:abort.signal,button(label,action,cls){const b=el('button',label,cls);b.action=action;buttons.push(b);return b;},header(){const h=el('header'),copy=el('div');copy.append(el('h1'));h.append(copy);return h;},icon:()=>el('svg'),dot:status=>el('span',undefined,`status-dot ${String(status).toLowerCase()}`),longPress:sandbox.longPress,groupRoute:(g,id)=>`/host/local/group/${g.key}/s/${id??g.sessions[0]?.id}`,go(){},rerender(){},savePreferences(p){Object.assign(state.preferences,p);},error(){},photo(){},transport:{},closeSheet(){},sheet:(title,entries,_,render)=>sheets.push({title,entries,render}),act:fn=>fn(),rpc:async req=>{calls.push(req);if(req.action==='chat_poll')return {agent:'claude',bound:true,next_offset:128,reset:false,items:[{kind:'user',text:'Fix bug'},{kind:'tool_call',tool_name:'Edit',title:'Update lib',file_path:'src/lib.rs',text:'src/lib.rs'},{kind:'question',title:'Mode',text:'Pick one',multi_select:false,options:[{label:'Fast',description:'Quick'},{label:'Safe',description:'Thorough'}]}]};if(req.action==='git')return {patch:'+added line'};return {sent:true};},refresh:async()=>{}};
 const root=el('main');
 const view=sandbox.KrillMobile.screens.session(root,ctx);
 assert.equal(mounts.length,1);
 const flush=async()=>{for(let i=0;i<10;i++)await Promise.resolve();};
 const toggleBtn=buttons.find(b=>b.className?.includes('mode-toggle'));
 // 기본 보기는 에이전트 세션(claude)도 터미널이다. Chat은 사용자가 고르면 열리고 세션별로 저장된다.
 assert.equal(toggleBtn.textContent,'Chat');
 await flush();assert(calls.some(c=>c.action==='chat_poll'&&c.latest===true),'prepare the latest chat window while Term is visible');
 const preparedFeed=root.descendants().find(n=>n.className==='chat-feed');
 const preparedQuestion=root.descendants().find(n=>n.className==='chat-card chat-question');
 buttons.find(b=>b.textContent==='Terminal actions').action();assert.equal(sheets.at(-1).entries[0].label,'Switch to Chat');sheets.at(-1).entries[0].action();assert.equal(toggleBtn.textContent,'Term');assert.equal(toggleBtn.getAttribute('aria-label'),'Term');assert.equal(state.preferences.sessionViewModes[7],'chat');assert.equal(mounts[0].visible,false);
 state.snapshot||={};state.snapshot.quick_commands=[{name:'Chat skill',text:'/ship-dev '}];assert(!root.descendants().some(n=>n.className==='btn chat-quick-actions-btn'),'chat uses the shared header quick actions');buttons.find(b=>b.className==='icon-button session-quick-actions').action();assert.equal(sheets.at(-1).entries.length,0,'terminal-only controls stay out of chat');const quickBody=el('div');sheets.at(-1).render(quickBody);const quick=quickBody.descendants().find(n=>n.className==='quick-action-insert'),beforeQuick=calls.filter(c=>c.action==='prompt').length;quick.action();assert.equal(root.descendants().find(n=>n.className==='chat-input').value,'/ship-dev ');assert.equal(state.drafts.get(7),'/ship-dev ');assert.equal(calls.filter(c=>c.action==='prompt').length,beforeQuick);

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
 buttons.find(b=>b.textContent==='Terminal actions').action();
 const selectTextEntry=sheets.at(-1).entries.find(e=>e.label==='Select text');assert(selectTextEntry);
 selectTextEntry.action();const textSheetBody=new Element('div');sheets.at(-1).render(textSheetBody);
 const screenTextArea=textSheetBody.descendants().find(n=>n.className==='screen-text');assert(screenTextArea);
 assert(screenTextArea.value.includes('Fix bug')&&screenTextArea.value.includes('Pick one'),'Chat mode Select text falls back to .chat-feed card texts');
 buttons.find(b=>b.textContent==='Terminal actions').action();assert.equal(sheets.at(-1).entries[0].label,'Switch to Terminal');sheets.at(-1).entries[0].action();
 assert.equal(toggleBtn.textContent,'Chat');assert.equal(mounts[0].visible,true);
 // [Chat | Term] 전환 시 TerminalAdapter를 언마운트하지 않고 그대로 유지해야 한다(0ms 전환).
 assert.equal(mounts.length,1);
 assert.equal(unmounts.length,0);
 toggleBtn.action();
 assert.equal(toggleBtn.textContent,'Term');
 assert.equal(root.descendants().find(n=>n.className==='chat-feed'),preparedFeed,'mode changes retain the prepared DOM');
 assert.equal(root.descendants().find(n=>n.className==='chat-card chat-question'),preparedQuestion);
 rawBtn.action();assert.equal(toggleBtn.textContent,'Chat');toggleBtn.action();
 assert.equal(mounts.length,1);
 assert.equal(unmounts.length,0);
 view.cleanup();
 assert.equal(unmounts.length,1);
 buttons.length=0;const savedView=sandbox.KrillMobile.screens.session(el('main'),ctx);assert.equal(buttons.find(b=>b.className?.includes('mode-toggle')).textContent,'Term');savedView.cleanup();
 // 일반 셸 세션(agent === '')은 기본값이 Term 뷰다.
 buttons.length=0;
 const shellRoot=el('main'),shellView=sandbox.KrillMobile.screens.session(shellRoot,{...ctx,route:{groupKey:'g',sessionId:8}});
 const shellToggle=buttons.find(b=>b.className?.includes('mode-toggle'));
 assert.equal(shellToggle.textContent,'Chat');
 const tabs=shellRoot.descendants().find(n=>n.className==='session-tabs');
 assert(!tabs.descendants().some(n=>n.className==='tab-quick'||n.className==='tab-action-divider'));
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
 const sandbox={URL,AbortController,Event,EventTarget,TextEncoder,TextDecoder,Uint8Array,DataView,BigInt,Promise,Math,Error,JSON,Date:{now:()=>now},crypto:{getRandomValues:b=>b.fill(1)},setTimeout:(fn,ms)=>{const id=++nextId;timers.set(id,{fn,at:now+Math.max(0,ms)});return id;},clearTimeout:id=>timers.delete(id),WebSocket:FakeSocket,MkCrypto:{enc:new TextEncoder(),concat:(...a)=>{const out=new Uint8Array(a.reduce((n,x)=>n+x.length,0));let i=0;for(const x of a){out.set(x,i);i+=x.length;}return out;},unhex:()=>new Uint8Array(32),hex:()=>'00'.repeat(32),sha256:a=>a,unbase64:()=>new Uint8Array(32),Channel:class{seal(a){return a;}open(a){return a;}}},KrillMobile:{}};
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
 const sandbox={URL,AbortController,Event,EventTarget,TextEncoder,TextDecoder,Uint8Array,DataView,BigInt,Promise,Math,Error,JSON,Number,document,Date:{now:()=>now},crypto:{getRandomValues:b=>b.fill(1)},setTimeout:(fn,ms)=>{const id=++nextId;timers.set(id,{fn,at:now+Math.max(0,ms)});return id;},clearTimeout:id=>timers.delete(id),WebSocket:FakeSocket,MkCrypto:{enc,concat,unhex:()=>new Uint8Array(32),hex:()=>'00'.repeat(32),sha256:a=>a,unbase64:()=>new Uint8Array(32),Channel:class{seal(a){return a;}open(a){return new Uint8Array(a);}}},KrillMobile:{}};
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
 // A healthy snapshot slower than 1.5s retains the existing authenticated socket.
 t.resume();await flush();assert.equal(sockets.length,4);assert.equal(socket.sent.length,6);
 for(const [id,tm] of [...timers])if(tm.at<=now+2000){timers.delete(id);tm.fn();}await flush();assert.equal(socket.closed,undefined);
 reply();await flush();assert.equal(snapshots.length,2);assert.equal(sockets.length,4);
 // The normal RPC deadline still closes a genuinely stalled socket and retries.
 t.resume();await flush();assert.equal(socket.sent.length,7);
 const rpcDeadline=[...timers.values()].find(tm=>tm.at===now+15000);assert(rpcDeadline);
 rpcDeadline.fn();await flush();assert.equal(socket.closed,true);
 for(const [id,tm] of [...timers])if(tm.at<=now){timers.delete(id);tm.fn();}await flush();
 assert.equal(socket.closed,true);assert.equal(sockets.length,5);assert.deepEqual(errors,[]);
 t.close();
}
if(require.main===module)main().catch(e=>{console.error(e);process.exitCode=1;});
module.exports={fixture,main};
