// Screen regressions only: deterministic DOM/transport, no app, daemon, accounts or PTY.
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const copyKeys=new Set();
function fixture(screen='orchestration',options={}){
 const doc={activeElement:null,hidden:false};
 class Element extends EventTarget{
  constructor(tag,text='',cls=''){super();this.tagName=tag.toUpperCase();this.children=[];this.parentElement=null;this.dataset={};this.attrs={};this.style={};this.className=cls;this._text=String(text??'');this.scrollTop=0;this.scrollLeft=0;this.value='';this.selectionStart=0;this.selectionEnd=0;this.moves=0;this.hidden=false;this.disabled=false;this.classList={add:(...v)=>{this.className=[...new Set([...this.className.split(' '),...v])].join(' ');},toggle:(v,force)=>{const values=new Set(this.className.split(' ').filter(Boolean)),on=force??!values.has(v);if(on)values.add(v);else values.delete(v);this.className=[...values].join(' ');return on;},contains:v=>this.className.split(' ').includes(v)};}
  get nextSibling(){if(!this.parentElement)return null;const a=this.parentElement.children;return a[a.indexOf(this)+1]||null;}
  get textContent(){return this._text+this.children.map(n=>n.textContent).join('');}
  set textContent(v){for(const c of this.children)c.remove();this._text=String(v??'');}
  get isConnected(){return this===doc.body||!!this.parentElement?.isConnected;}
  append(...nodes){for(const n of nodes)this.insertBefore(n,null);}
  prepend(...nodes){for(const n of [...nodes].reverse())this.insertBefore(n,this.children[0]||null);}
  insertBefore(n,before){if(before&&before.parentElement!==this)throw Error('Invalid insertBefore anchor');if(n===before)return;if(n.parentElement){n.moves++;n.remove();}n.parentElement=this;const at=before?this.children.indexOf(before):this.children.length;this.children.splice(at,0,n);}
  remove(){if(this.contains(doc.activeElement))doc.activeElement=doc.body;if(this.parentElement)this.parentElement.children=this.parentElement.children.filter(n=>n!==this);this.parentElement=null;}
  replaceChildren(...nodes){for(const c of [...this.children])c.remove();this._text='';this.append(...nodes);}
  contains(n){return n===this||this.children.some(c=>c.contains(n));}
  setAttribute(k,v){this.attrs[k]=String(v);}
  getAttribute(k){return this.attrs[k]??null;}
  descendants(){return this.children.flatMap(n=>[n,...n.descendants()]);}
  matches(selector){if(selector.startsWith('.'))return this.classList.contains(selector.slice(1));if(selector.startsWith('#'))return this.id===selector.slice(1);if(selector==='textarea,input')return ['TEXTAREA','INPUT'].includes(this.tagName);return this.tagName===selector.toUpperCase();}
  querySelector(selector){return this.descendants().find(n=>n.matches(selector))||null;}
  querySelectorAll(selector){return this.descendants().filter(n=>n.matches(selector));}
  focus(){doc.activeElement=this;}
  blur(){if(doc.activeElement===this)doc.activeElement=doc.body;}
  select(){this.focus();this.selectionStart=0;this.selectionEnd=this.value.length;}
  scrollIntoView(opts){this.scrolledIntoView=opts||true;}
 }
 doc.body=new Element('body');doc.activeElement=doc.body;doc.createElementNS=(_,tag)=>new Element(tag);const root=new Element('main');doc.body.append(root);
 let timerId=0;const timers=new Map(),calls=[],errors=[],routes=[],headers=[],sheets=[],saved=[],buttons=[],abort=new AbortController();const state={snapshot:{host:{display_name:'Test desktop'},orchestration:{gates:[],questions:[],runs:[],tasks:[]},repositories:[]},connection:{phase:'ready'},groups:new Map(),sessions:new Map(),drafts:new Map(),preferences:{},logs:[],ui:{search:'',filter:'all'},...options.state};
 const el=(...args)=>new Element(...args),ctx={el,state,signal:abort.signal,route:options.route||{},t:(s,...values)=>{copyKeys.add(s);return options.t?.(s,...values)??s;},go:r=>routes.push(r),backTo:r=>routes.push(r),longPress:(node,action)=>{node._longPress=action;return ()=>{};},groupRoute:(g,id)=>`/host/local/group/${g?.key}/s/${id}`,rollup:s=>['Waiting','Blocked','Working','Done','Exited','Unknown'].find(v=>s.some(x=>(x.exited?'Exited':x.state)===v))||'Unknown',icon:()=>el('svg'),dot:v=>el('span',undefined,`status-dot ${String(v).toLowerCase()}`),button(label,action,cls=''){const b=el('button',ctx.t(label),cls);b.setAttribute('aria-label',ctx.t(label));b.action=action;buttons.push(b);return b;},header(title,fallback,onBack){headers.push({title,fallback,onBack});const h=el('header'),copy=el('div',undefined,'header-copy');copy.append(el('h1',ctx.t(title)));h.append(copy);return h;},linkRow(title,description,route){const b=ctx.button('',()=>ctx.go(route),'setting-link'),copy=el('span',undefined,'link-copy');copy.append(el('strong',ctx.t(title)));if(description)copy.append(el('small',ctx.t(description)));b.append(copy);return b;},sheet:(...v)=>sheets.push(v),act:fn=>Promise.resolve().then(fn).catch(e=>errors.push(e)),rpc:async req=>{calls.push(req);return options.rpc?.(req)??{};},createSession:(group,agent,req)=>{calls.push(req);routes.push(`/host/local/group/${group.key}/s/new`);},refresh:async()=>options.refresh?.(),error:e=>errors.push(e),savePreferences:patch=>{saved.push(patch);Object.assign(state.preferences,patch);},rerender(){ctx.rerenders++;},rerenders:0,scanQr(){ctx.scans++;},scans:0,pasteLink(){},install:{show(){ctx.installShown++;}},installShown:0,transport:{reconnect(){},resume(){}}};
 ctx.getConnections=()=>ctx.connections||[];ctx.selectHost=(id,after)=>{ctx.selected=id;after?.();};ctx.openRecent=(id,locator)=>{ctx.opened={id,locator};};ctx.reconnectHost=id=>{ctx.reconnected=id;};ctx.renameHost=id=>{ctx.renamed=id;};ctx.forgetHost=id=>{ctx.forgot=id;};
 const sandbox={document:doc,AbortController,Date,console,setTimeout:(fn,ms)=>{timers.set(++timerId,{fn,ms});return timerId;},clearTimeout:id=>timers.delete(id),setInterval:(fn,ms)=>{timers.set(++timerId,{fn,ms,interval:true});return timerId;},clearInterval:id=>timers.delete(id),KrillMobile:{terminal:{fontScales:[1,1.25],keys:[{id:'esc',label:'Esc'},{id:'tab',label:'Tab'}]},install:options.install||{offer:null}},navigator:{},...options.globals};vm.createContext(sandbox);for(const file of ['details','home','workspaces','session','settings','bots'])vm.runInContext(fs.readFileSync(path.join(options.assetRoot||path.join(__dirname,'../src/mobile/assets/screens'),`${file}.js`),'utf8'),sandbox);
 const view=sandbox.KrillMobile.screens[screen](root,ctx),all=()=>root.descendants(),find=(selector)=>root.querySelector(selector),byText=(label)=>all().find(n=>n.tagName==='BUTTON'&&n.getAttribute('aria-label')===label),flush=async()=>{for(let i=0;i<40;i++)await Promise.resolve();};
 return {root,ctx,state,doc,view,calls,errors,routes,headers,sheets,saved,buttons,all,find,byText,flush,timers,abort,M:sandbox.KrillMobile};
}
const deferred=()=>{let resolve,reject;const promise=new Promise((y,n)=>{resolve=y;reject=n;});return {promise,resolve,reject};};
const emit=(node,type)=>node.dispatchEvent(new Event(type,{cancelable:true}));
async function orchestration(){
 let pending=deferred();const q={id:'question-1',summary:'같은 질문',body:'설명을 읽고 답하세요',run_id:'run-1',task_id:'task-1',from:'worker-a',to:'user'},run={id:'run-1',title:'배포 준비',status:'active'},task={id:'task-1',run_id:'run-1',title:'변경 검토',status:'running'};
 const f=fixture('orchestration',{state:{snapshot:{orchestration:{questions:[q],gates:[],runs:[run],tasks:[task]}}},rpc:req=>req.action==='question_reply'?pending.promise:{}}),input=f.find('textarea'),list=f.find('#orchestration');input.focus();input.value='한글 조합 중인 초안';input.selectionStart=3;input.selectionEnd=6;emit(input,'input');list.scrollTop=177;const moves=input.parentElement.parentElement.moves;
 const gate={id:'gate-1',question:'계속 진행할까요?',run_id:'run-1',task_id:'task-1',from:'worker-b',status:'pending',options:['Approve','Deny']};f.state.snapshot.orchestration.gates=[gate];f.state.snapshot.orchestration.tasks.push({id:'done',run_id:run.id,title:'Completed task',status:'completed'});f.view.update();assert.equal(f.find('textarea'),input);assert.equal(f.doc.activeElement,input);assert.equal(input.value,'한글 조합 중인 초안');assert.equal(input.selectionStart,3);assert.equal(input.selectionEnd,6);assert.equal(input.parentElement.parentElement.moves,moves);assert.equal(list.scrollTop,177);assert(f.root.textContent.includes('계속 진행할까요?'));assert(f.root.textContent.includes('1 / 2 tasks complete'));assert(f.root.textContent.includes('Run: 배포 준비 · Task: 변경 검토 · From: worker-b'));
 // Same snapshot ordering and stable IDs must not move or rebuild an active IME input.
 for(let i=0;i<50;i++){run.title='배포 준비 '+i;f.view.update();}assert.equal(f.doc.activeElement,input);assert.equal(f.find('textarea'),input);assert.equal(input.parentElement.parentElement.moves,moves);
 f.state.connection.phase='disconnected';f.view.update();assert.equal(f.byText('Approve').disabled,true);await f.byText('Approve').action();assert.equal(f.calls.length,0);assert(f.root.textContent.includes('Reconnect to reply.'));f.state.connection.phase='ready';f.view.update();
 pending=deferred();f.ctx.rpc=async req=>{f.calls.push(req);return pending.promise;};const first=f.byText('Approve').action(),second=f.byText('Deny').action();assert.equal(f.calls.length,1);assert.equal(f.byText('Deny').disabled,true);assert.equal(f.byText('Approve').disabled,true);pending.resolve({});await Promise.all([first,second]);assert(f.root.textContent.includes('Decision sent.'));assert(f.byText('Approve').disabled);
 // Removing a gate in a concurrent snapshot makes a retained button stale and harmless.
 const staleApprove=f.byText('Approve');f.state.snapshot.orchestration.gates=[];f.view.update();assert.equal(f.byText('Approve'),undefined);await staleApprove.action();assert.equal(f.calls.length,1);
 pending=deferred();emit(input.parentElement,'submit');await f.flush();assert.equal(f.calls.length,2);assert.equal(f.calls[1].body,'한글 조합 중인 초안');input.value='전송 중 새 초안';emit(input,'input');pending.resolve({});await f.flush();assert.equal(f.state.drafts.get(q.id),'전송 중 새 초안');assert.equal(input.value,'전송 중 새 초안');assert(f.root.textContent.includes('Reply sent.'));assert.equal(f.doc.activeElement,input);
 assert.equal(f.headers[0].fallback,'/');f.abort.abort();
 const g=fixture('orchestration',{state:{snapshot:{orchestration:{gates:[gate],questions:[],runs:[run],tasks:[task]}}},rpc:()=>Promise.reject(Error('Already resolved elsewhere'))});await g.byText('Approve').action();assert(g.root.textContent.includes('Already resolved elsewhere'));assert.equal(g.byText('Approve').disabled,false);g.abort.abort();
 const reply=deferred(),refresh=deferred(),h=fixture('orchestration',{state:{snapshot:{orchestration:{questions:[q],runs:[run],tasks:[task]}}},rpc:()=>reply.promise,refresh:()=>refresh.promise}),area=h.find('textarea');area.value='reply';emit(area,'input');emit(area.parentElement,'submit');await h.flush();for(const timer of [...h.timers.values()])timer.fn();assert(h.root.textContent.includes('Sending…'));reply.resolve({});await h.flush();assert(h.root.textContent.includes('Reply sent.'));assert.equal(area.parentElement.querySelector('button').disabled,true);assert.equal(area.value,'');refresh.reject(Error('refresh failed'));await h.flush();assert(h.root.textContent.includes('Refresh to check the latest status.'));h.abort.abort();
 const late=deferred(),j=fixture('orchestration',{state:{snapshot:{orchestration:{questions:[q],runs:[run],tasks:[task]}}},rpc:()=>late.promise}),old=j.find('textarea');old.value='sent before unmount';emit(old,'input');emit(old.parentElement,'submit');await j.flush();j.abort.abort();j.state.drafts.set(q.id,'draft from replacement screen');late.resolve({});await j.flush();assert.equal(j.state.drafts.get(q.id),'draft from replacement screen');
 console.log('PASS SC-A01/A02/A03/A04/A05: keyed IME input, simultaneous gates/run updates, context, offline/pending/success/error and stale replies');
}
async function confirmationInbox(){
 const active={id:'current',title:'Current project',status:'active'},finished={id:'old',title:'Old project',status:'completed'},archived={id:'archived',title:'Archived project',status:'archived'},task={id:'review',run_id:'current',title:'Review',status:'blocked'},done={id:'done',run_id:'current',status:'completed'};
 const q={id:'human',from:'coordinator',to:'user',run_id:'current',task_id:'review',summary:'Choose the release scope',body:'Include the documentation fix?'},gate={id:'decision',status:'pending',run_id:'current',question:'Publish this release?',options:['Approve','Deny']};
 const store={runs:[active,finished,archived],tasks:[task,done],questions:[q,{...q,id:'internal',to:'coordinator',summary:'Internal worker exchange'},{...q,id:'answered',acked:true},{...q,id:'finished',run_id:'old',task_id:null},{...q,id:'orphan',run_id:'missing',task_id:null},{...q,id:'done',task_id:'done'}],gates:[gate,{...gate,id:'old-gate',run_id:'old'},{...gate,id:'archive-gate',run_id:'archived'},{...gate,id:'resolved',status:'resolved'}]};
 const f=fixture('orchestration',{state:{snapshot:{orchestration:store}}});assert.equal(f.headers[0].title,'Confirmation requests');assert.equal(f.all().filter(n=>n.classList.contains('gate-card')).length,2);assert(!f.root.textContent.includes('Internal worker exchange'));assert(!f.root.textContent.includes('Old project'));assert(!f.root.textContent.includes('Archived project'));const details=f.find('.gate-card').querySelector('details');assert(!details.open,'technical context starts collapsed');const input=f.find('textarea'),form=input.parentElement;input.value='Include the fix';emit(input,'input');active.status='completed';f.view.update();assert.equal(f.find('textarea'),null);assert.equal(f.find('.gate-card'),null);assert(f.root.textContent.includes('No requests waiting for you.'));emit(form,'submit');await f.flush();assert.equal(f.calls.length,0,'a stale detached form cannot submit to a finished run');assert.equal(store.questions.find(q=>q.id==='internal').acked,undefined,'projection never acknowledges worker mail');f.abort.abort();active.status='active';
 const h=fixture('bots'),one={id:'one',name:'First PC',connection:{phase:'ready'},snapshot:{capabilities:{bots:true},orchestration:store,sessions:[]}},two={id:'two',name:'Second PC',connection:{phase:'ready'},snapshot:{capabilities:{bots:true},orchestration:{gates:[{id:'decision',status:'pending'}]},sessions:[]}};h.ctx.connections=[one,two];h.view.update();const banner=()=>h.find('.todo-summary-banner');assert(banner(),'Bots home shows 1-line summary banner when requests exist');assert(banner().textContent.includes('3 requests need your attention'));await banner().action();assert.equal(h.routes.at(-1),'/up-next','banner routes straight to To Do tab');one.snapshot.orchestration.questions=[];one.snapshot.orchestration.gates=[];two.snapshot.orchestration.gates=[];h.view.update();assert(!banner(),'Bots home hides summary banner when 0 requests remain');h.abort.abort();
 const w=fixture('workspaces');assert.equal(w.byText('Orchestration'),undefined);assert(w.find('#workspace-confirmation-requests').hidden);w.state.snapshot.orchestration={gates:[{id:'decision',status:'pending'}]};w.view.update();const entry=w.find('#workspace-confirmation-requests');assert(!entry.hidden);await entry.action();assert.equal(w.routes.at(-1),'/up-next');w.abort.abort();
 console.log('PASS confirmation inbox: human-only current requests, retired work, stale form rejection, collapsed context, Bots home banner, cached/offline disabling and contextual workspace entry');
}

function groups(f){const a={id:1,state:'Waiting',agent:'codex',branch:'main',groupKey:'g-a',lines:['확인 필요']},b={id:2,state:'Working',branch:'dev',groupKey:'g-b',lines:[]};const ga={key:'g-a',label:'한글 워크스페이스',path:'/tmp/a',repoKey:'repo',sessions:[a]},gb={key:'g-b',label:'Other',path:'/tmp/b',repoKey:'repo',sessions:[b]};f.state.groups=new Map([[ga.key,ga],[gb.key,gb]]);f.state.sessions=new Map([[a.id,a],[b.id,b]]);f.state.snapshot.repositories=[{key:'repo',label:'프로젝트'}];f.state.preferences.workspaceTree={'repo:repo':true,...f.state.preferences.workspaceTree};return {a,b,ga,gb};}
async function workspace(){const f=fixture('workspaces');const {a,ga}=groups(f);f.view.update();const row=f.all().find(n=>n.dataset.group==='g-a'),list=f.find('.workspace-list');row.focus();list.scrollTop=123;const initial=row.moves;a.lines=['갱신된 내용'];ga.label='긴 한글 '.repeat(70);f.view.update();assert.equal(f.all().find(n=>n.dataset.group==='g-a'),row);assert.equal(f.doc.activeElement,row);assert.equal(row.moves,initial);assert.equal(list.scrollTop,123);assert(row.getAttribute('aria-label').includes('Waiting'));assert(!row.textContent.includes('갱신된 내용'),'terminal output does not crowd the workspace list');assert(row.getAttribute('aria-label').includes(ga.label));await row.action();assert(f.routes.at(-1).endsWith('/s/1'));
 // Orca host page: the search pill stays in the bottom bar next to options and New workspace.
 const input=f.all().find(n=>n.tagName==='INPUT'&&n.type==='search'),bar=f.find('.workspace-bottom-bar');assert(bar.contains(input));assert.deepEqual(bar.children.map(n=>n.getAttribute('aria-label')||n.className),['Workspace Options','workspace-search','New workspace']);assert(!f.byText('Search'),'phone search needs no toggle');input.value='Other';emit(input,'input');assert.equal(f.all().filter(n=>n.dataset.group).length,1);input.value='';emit(input,'input');assert.equal(f.state.ui.search,'');assert.equal(f.all().filter(n=>n.dataset.group).length,2);
 input.value='missing';emit(input,'input');assert(f.root.textContent.includes('No matching workspaces'));await f.byText('Clear search').action();assert.equal(f.state.ui.search,'');assert.equal(f.doc.activeElement,input);input.value='Other';emit(input,'input');const escape=new Event('keydown',{cancelable:true});escape.key='Escape';input.dispatchEvent(escape);assert.equal(f.state.ui.search,'');assert.equal(f.doc.activeElement,input);assert(!input.parentElement.hidden);assert.equal(f.all().filter(n=>n.dataset.group).length,2);const ranked=['Unknown','Exited','Done','Working','Blocked','Waiting'].map((state,i)=>({key:'rank-'+state,label:state,path:'/tmp/'+state,sessions:[{id:10+i,state}]}));f.state.preferences.workspaceTree.folders=true;f.state.groups=new Map(ranked.map(g=>[g.key,g]));f.state.sessions=new Map(ranked.map(g=>[g.sessions[0].id,g.sessions[0]]));f.view.update();assert.deepEqual(f.all().filter(n=>n.dataset.group).map(n=>n.dataset.group),['rank-Waiting','rank-Blocked','rank-Working','rank-Done','rank-Unknown']);f.abort.abort();
 console.log('PASS SC-A04/A06/A14 + SC-Q05 support: keyed workspace focus/scroll/latest callback, full context, search close/clear/Escape');}
async function workspaceTree(){
 const f=fixture('workspaces');const {a,ga}=groups(f);a.title='✳ 리뷰 정리';a.lines=['마지막 출력'];const exited={id:3,state:'Done',exited:true};ga.sessions.push(exited);f.view.update();const folds=()=>JSON.parse(JSON.stringify(f.state.preferences.workspaceTree)),sessions=()=>f.all().filter(n=>n.dataset.session).map(n=>n.dataset.session),toggle=()=>f.all().find(n=>n.getAttribute('aria-label')===`Terminals: ${ga.label}`);
 assert.deepEqual(sessions(),[],'phone worktrees start folded (Orca lists workspaces only)');assert.equal(toggle().getAttribute('aria-expanded'),'false');await toggle().action();assert.deepEqual(sessions(),['1'],'opening one worktree shows only its live terminals');assert.equal(toggle().getAttribute('aria-expanded'),'true');assert.deepEqual(folds(),{'repo:repo':true,'w:g-a':true},'only the opened worktree is stored');
 const row=f.all().find(n=>n.dataset.session==='1');assert(row.textContent.includes('마지막 출력'));assert.equal(row.getAttribute('aria-label'),'리뷰 정리 · Waiting for you · 마지막 출력');assert(row.descendants().some(n=>n.getAttribute('class')==='agent-icon codex'),'session row reuses the shared provider mark');a.lines=['새 출력','❯ '];f.view.update();assert.equal(f.all().find(n=>n.dataset.session==='1'),row,'session rows are keyed');assert(row.textContent.includes('새 출력'),'prompt-only trailing line is skipped');await row.action();assert(f.routes.at(-1).endsWith('/s/1'));
 f.state.ui.search='리뷰 정리';f.view.update();assert.deepEqual(f.all().filter(n=>n.dataset.group).map(n=>n.dataset.group),['g-a'],'search matches session title');f.state.ui.search='codex';f.view.update();assert.deepEqual(f.all().filter(n=>n.dataset.group).map(n=>n.dataset.group),['g-a'],'search matches session agent');f.state.ui.search='새 출력';f.view.update();assert.deepEqual(f.all().filter(n=>n.dataset.group).map(n=>n.dataset.group),['g-a'],'search matches session activity line');f.state.ui.search='';f.view.update();
 const hostDot=f.find('.host-subtitle').querySelector('.status-dot');assert.equal(hostDot.className,'status-dot connected');f.state.connection.phase='connecting';f.view.update();assert.equal(hostDot.className,'status-dot working');f.state.connection.phase='denied';f.view.update();assert.equal(hostDot.className,'status-dot blocked');f.state.connection.phase='ready';f.view.update();
 await toggle().action();assert.deepEqual(sessions(),[]);assert.deepEqual(folds(),{'repo:repo':true},'default terminal folds are not stored');await toggle().action();assert.deepEqual(sessions(),['1']);
 const project=f.all().find(n=>n.classList.contains('repo-toggle'));assert.equal(project.getAttribute('aria-expanded'),'true');await project.action();await f.flush();assert.equal(project.getAttribute('aria-expanded'),'false');assert.equal(f.all().filter(n=>n.dataset.group).length,0);assert.deepEqual(folds(),{'repo:repo':false,'w:g-a':true},'project folds stay on this phone');assert(!f.calls.some(c=>c.action==='set_workspace_view'),'nothing is sent to the PC');
 f.state.ui.search='Other';f.view.update();assert.equal(f.all().filter(n=>n.dataset.group).length,1,'search opens collapsed projects');f.abort.abort();
 const g=fixture('workspaces',{state:{preferences:{workspaceTree:{'repo:repo':true,'w:g-a':true}}}});groups(g);g.view.update();assert.deepEqual(g.all().filter(n=>n.dataset.session).map(n=>n.dataset.session),['1'],'saved folds restore');g.abort.abort();
 // PC 사이드바의 접힘은 따라가지 않는다(기기별 저장). 폰에서 접은 프로젝트만 이 폰에 남는다.
 const d=fixture('workspaces');groups(d);d.state.snapshot.repositories.push({key:'closed',label:'Closed on PC',collapsed:true});d.state.groups.set('g-c',{key:'g-c',label:'C',path:'/tmp/c',repoKey:'closed',sessions:[]});d.view.update();const repoToggles=()=>d.all().filter(n=>n.classList.contains('repo-toggle')),shownGroups=()=>d.all().filter(n=>n.dataset.group).map(n=>n.dataset.group);
 assert.deepEqual(repoToggles().map(n=>n.getAttribute('aria-expanded')),['true','true'],'a fold on the PC does not close the project on the phone');assert.deepEqual(shownGroups(),['g-a','g-b','g-c']);
 await repoToggles()[1].action();await d.flush();assert.deepEqual(shownGroups(),['g-a','g-b']);assert.equal(d.state.preferences.workspaceTree['repo:closed'],false);assert(!d.calls.length,'folding sends nothing to the PC');d.abort.abort();
 console.log('PASS workspace tree: project and worktree folds stay on the phone and non-default, keyed session rows route, search opens projects');
}
async function emptyWorkspace(){
 const f=fixture('workspaces',{rpc:()=>({session:99})}),g={key:'empty',label:'PC 이름',path:'/repo/empty',repoKey:'repo',pinned:true,sessions:[]};groups(f);f.state.groups.set(g.key,g);f.state.snapshot.capabilities={workspace_create:true};f.state.snapshot.agents=[{id:'codex',name:'Codex'}];f.view.update();const row=f.all().find(n=>n.dataset.group===g.key);assert(row);assert.equal(row.querySelector('.terminal-count').textContent,'0');await row.action();assert.equal(f.routes.length,0);assert.equal(f.sheets.at(-1)[0],'New session');const sheetBody=new f.root.constructor('div');f.sheets.at(-1)[3](sheetBody);await sheetBody.querySelector('.new-tab-tile').action();assert.deepEqual(JSON.parse(JSON.stringify(f.calls)),[{action:'create_terminal',workspace:g.key,agent:'codex'}]);assert(f.routes.at(-1).endsWith('/s/new'));f.abort.abort();
 // Execute the app's actual grouping function: empty catalog entries survive and attach sessions by key.
 const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8'),line=source.split('\n').find(l=>l.includes('function group(data)')),sandbox={state:{},M:f.M};vm.createContext(sandbox);vm.runInContext(line+'globalThis.groupSnapshot=group;',sandbox);sandbox.groupSnapshot({workspaces:[{key:'a',label:'PC 이름',path:'/repo/a',repo_key:'r',pinned:true,primary:true,depth:2,icon:{kind:'emoji',value:'🚀'}},{key:'empty',label:'빈 작업 공간',path:'/repo/empty',repo_key:'r'}],sessions:[{id:1,worktree_key:'a',group_label:'폴더명',cwd:'/repo/a'}]});assert.equal(sandbox.state.groups.size,2);assert.equal(sandbox.state.groups.get('empty').sessions.length,0);assert.equal(sandbox.state.groups.get('a').label,'PC 이름');assert.equal(sandbox.state.groups.get('a').depth,2);assert.equal(sandbox.state.groups.get('a').primary,true);assert.equal(sandbox.state.groups.get('a').icon.value,'🚀');assert.equal(sandbox.state.groups.get('a').sessions[0].id,1);sandbox.hostRoute=()=>'/host/desktop';vm.runInContext(source.split('\n').find(l=>l.includes('const groupRoute='))+'globalThis.routeFor=groupRoute;',sandbox);assert.equal(sandbox.routeFor(sandbox.state.groups.get('empty')),'/host/desktop');assert(sandbox.routeFor(sandbox.state.groups.get('empty'),99).endsWith('/s/99'));sandbox.groupSnapshot({sessions:[{id:2,worktree_key:'old',group_label:'구버전'}]});assert.equal(sandbox.state.groups.size,1);assert.equal(sandbox.state.groups.get('old').label,'구버전');
 console.log('PASS workspace catalog: sessionless rows, PC names, session grouping, legacy snapshot and scoped session creation');
}
// Orca 작업 공간 옵션: PC와 같은 기능으로 프로젝트·작업 공간을 숨기고 묶고 정렬한다. 바꾸면 바로 다시 그리고 이 폰에만 저장한다. 숨은 항목도 검색하면 보인다.
async function desktopVisibility(){
 const f=fixture('workspaces'),source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8'),sandbox={state:f.state,M:{}};vm.createContext(sandbox);vm.runInContext(source.split('\n').find(l=>l.includes('function group(data)'))+'globalThis.groupSnapshot=group;',sandbox);f.state.preferences.workspaceTree={};f.state.preferences.workspaceViews={'workspace-view':{hide_sleeping:true}};
 const facts=(o={})=>({asleep:true,default_branch:false,automation:false,external:false,external_shown:false,...o});
 const data={repositories:[{key:'a',label:'Shown'},{key:'b',label:'Other'},{key:'c',label:'Removed',removed:true}],workspaces:[{key:'a-main',repo_key:'a',label:'main',primary:true,status:'todo',orders:[2,2,0,2],visibility:facts({default_branch:true})},{key:'a-idle',repo_key:'a',label:'idle',status:'in-progress',orders:[0,0,3,0],visibility:facts()},{key:'a-busy',repo_key:'a',label:'busy',parent:'a-idle',status:'in-progress',orders:[1,1,1,1],visibility:facts({asleep:false})},{key:'b-main',repo_key:'b',label:'b main',primary:true,status:'completed',orders:[3,3,2,3],visibility:facts({default_branch:true})},{key:'c-main',repo_key:'c',label:'c main',primary:true,orders:[4,4,4,4],visibility:facts({default_branch:true})}],sessions:[]};
 sandbox.groupSnapshot(data);f.view.update();const sections=()=>f.all().filter(n=>n.classList.contains('repo-toggle')&&!n.parentElement.hidden).map(n=>n.querySelector('.repo-label').textContent),shown=()=>f.all().filter(n=>n.dataset.group).map(n=>n.dataset.group),chip=f.all().find(n=>n.getAttribute('aria-controls')==='workspace-filters'),row=label=>f.all().find(n=>n.tagName==='LABEL'&&n.children[0].textContent===label),toggle=async(label,on)=>{const box=row(label).children[1];box.checked=on;emit(box,'change');await f.flush();},saved=o=>{const v=JSON.parse(JSON.stringify(f.state.preferences.workspaceViews['workspace-view']));for(const [k,value] of Object.entries(o))assert.deepEqual(v[k],value,k);};
 assert.deepEqual(sections(),['Shown','Other'],'projects removed on the PC stay hidden');assert.deepEqual(shown(),['a-main','a-busy','b-main'],'sleeping worktrees hide except the default branch');assert.equal(f.find('.chip-badge').textContent,'1');assert.equal(chip.getAttribute('aria-label'),'Workspace options (1 filters)');assert(f.root.textContent.includes('2 projects · 3 workspaces'));
 await chip.action();assert.equal(f.find('#workspace-filters').hidden,false);assert(f.root.textContent.includes('Saved on this device'));assert.equal(row('Hide sleeping worktrees').children[1].checked,true);assert.equal(row('Always show default branch').hidden,false);
 await toggle('Hide sleeping worktrees',false);assert.deepEqual(shown(),['a-main','a-idle','a-busy','b-main'],'the list changes at once');saved({hide_sleeping:false});assert.equal(f.find('.chip-badge').hidden,true);assert.equal(row('Always show default branch').hidden,true);
 await f.find('.project-picker').action();assert.equal(f.find('#workspace-project-choices').hidden,false);assert.deepEqual(f.find('#workspace-project-choices').children.map(n=>n.children[0].textContent),['All projects','Shown','Other'],'removed projects are not offered');
 const pSearch=f.find('.project-filter-search');assert.equal(pSearch.placeholder,'Filter projects...');assert.equal(f.find('.project-filter-clear').hidden,true);assert.equal(f.find('.project-pills').hidden,true);
 await toggle('Other',true);saved({repos:['b']});assert.deepEqual(sections(),['Other']);assert.deepEqual(shown(),['b-main']);assert.equal(f.find('.project-picker').textContent,'ProjectsOther');assert.equal(f.find('.project-filter-title').textContent,'Projects · 1');assert.equal(pSearch.placeholder,'Add project...');assert.equal(f.find('.project-pills').hidden,false);assert.equal(f.byText('Remove Other filter').querySelector('.project-pill-name').textContent,'Other');
 await toggle('Shown',true);saved({repos:['a','b']});assert.equal(f.find('.project-picker').textContent,'Projects2 projects');assert.equal(f.find('.project-filter-title').textContent,'Projects · 2');
 await f.byText('Remove Shown filter').action();await f.flush();saved({repos:['b']});assert.equal(f.find('.project-picker').textContent,'ProjectsOther');
 pSearch.value='missing';emit(pSearch,'input');assert.equal(f.find('#workspace-project-choices').textContent,'No projects match');
 pSearch.value='Shown';emit(pSearch,'input');assert.deepEqual(f.find('#workspace-project-choices').children.map(n=>n.children[0].textContent),['Shown']);const enter=new Event('keydown',{cancelable:true});enter.key='Enter';pSearch.dispatchEvent(enter);await f.flush();assert.equal(pSearch.value,'');saved({repos:['a','b']});
 const bs=new Event('keydown',{cancelable:true});bs.key='Backspace';pSearch.dispatchEvent(bs);await f.flush();saved({repos:['a']});
 await toggle('Other',true);await toggle('Shown',false);saved({repos:['b']});
 f.state.ui.search='idle';f.view.update();assert.deepEqual(shown(),['a-idle'],'search still reaches hidden workspaces');f.state.ui.search='';
 await f.byText('Clear').action();await f.flush();saved({repos:[]});assert.deepEqual(sections(),['Shown','Other']);assert.equal(f.find('.project-picker').textContent,'ProjectsAll projects');
 await toggle('Other',true);await toggle('All projects',true);saved({repos:[]});assert.deepEqual(sections(),['Shown','Other']);
 await f.byText('Status').action();await f.flush();saved({group_by:1});assert.deepEqual(sections(),['Todo','In Progress','Completed']);assert.deepEqual(shown(),['a-main','a-idle','a-busy','b-main']);assert.equal(f.byText('Status').getAttribute('aria-pressed'),'true');
 await f.byText('None').action();await f.flush();assert.deepEqual(sections(),[],'no grouping shows one list without headers');assert.deepEqual(shown(),['a-idle','a-busy','a-main','b-main'],'desktop sort order across projects');assert(f.all().filter(n=>n.classList.contains('tree-node')).every(n=>n.dataset.depth==='0'));
 await f.byText('Name').action();await f.flush();saved({sort_by:2});assert.equal(f.byText('Name').getAttribute('aria-pressed'),'true');assert.deepEqual(shown(),['a-main','a-busy','b-main','a-idle'],'the phone re-sorts with its own sort option');
 await f.byText('Project').action();await f.flush();assert.deepEqual(shown(),['a-main','a-idle','a-busy','b-main'],'children follow their parent after re-sorting');assert.equal(f.all().find(n=>n.classList.contains('tree-node')&&n.querySelector('.workspace-row').dataset.group==='a-busy').dataset.depth,'1');
 await toggle('Hide CLI worktrees',true);assert.equal(f.find('.chip-badge').textContent,'1');await f.byText('Reset filters').action();await f.flush();saved({hide_sleeping:false,always_show_default_branch:true,hide_automation:false,hide_cli:false,repos:[]});assert.equal(f.find('.chip-badge').hidden,true);assert(!f.calls.length,'options never reach the PC');f.abort.abort();
 // 다시 열어도 이 폰에 저장한 옵션이 남고, 저장한 적 없는 폰은 데스크톱 기본값(모두 보임·프로젝트별·최근순)으로 시작한다.
 const kept=fixture('workspaces',{state:{preferences:{workspaceTree:{},workspaceViews:JSON.parse(JSON.stringify(f.state.preferences.workspaceViews))}}}),keptBox={state:kept.state,M:{}};vm.createContext(keptBox);vm.runInContext(source.split('\n').find(l=>l.includes('function group(data)'))+'globalThis.groupSnapshot=group;',keptBox);keptBox.groupSnapshot(data);kept.view.update();assert.equal(kept.byText('Name').getAttribute('aria-pressed'),'true');assert.equal(kept.byText('Project').getAttribute('aria-pressed'),'true');kept.abort.abort();
 const fresh=fixture('workspaces'),freshBox={state:fresh.state,M:{}};vm.createContext(freshBox);vm.runInContext(source.split('\n').find(l=>l.includes('function group(data)'))+'globalThis.groupSnapshot=group;',freshBox);freshBox.groupSnapshot(data);fresh.view.update();assert.deepEqual(fresh.all().filter(n=>n.dataset.group).map(n=>n.dataset.group),['a-main','a-idle','a-busy','b-main']);assert.equal(fresh.find('.chip-badge').hidden,true);assert.equal(fresh.byText('Recent').getAttribute('aria-pressed'),'true');fresh.abort.abort();
 // Warm-cache snapshot sanitizer parity across Home and Workspaces + NaN-free rank comparator.
 const storageCtx=vm.createContext({TextEncoder,TextDecoder,Uint8Array,DataView,URL,URLSearchParams,AbortController,MkCrypto:{enc:new TextEncoder(),hex:()=>'00',sha256:()=>new Uint8Array(32)}});
 vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/storage.js'),'utf8'),storageCtx);
 const warmInput={
  host:{display_name:'Studio',platform:'macos'},
  repositories:[{key:'a',label:'Shown',path:'/a',remote:'craftowen/krill',badge_color:'#3b82f6',icon:{kind:'emoji',value:'🚀'}},{key:'b',label:'Other',path:'/b'},{key:'c',label:'Removed',path:'/c',removed:true}],
  workspaces:[
   ...data.workspaces,
   {key:'a-ext-hidden',repo_key:'a',label:'.claude/wt',status:'in-review',orders:[5,5,5,5],visibility:facts({asleep:false,external:true,external_shown:false})}
  ],
  sessions:[]
 };
 const cachedData=storageCtx.KrillMobile.storage.metadata(warmInput);
 for(const groupBy of [0,1,3]){
  const renderPair=snap=>{const w=fixture('workspaces',{state:{cached:snap===cachedData,preferences:{workspaceTree:{},workspaceViews:{'workspace-view':{group_by:groupBy,hide_sleeping:true}}}}}),box={state:w.state,M:{}};vm.createContext(box);vm.runInContext(source.split('\n').find(l=>l.includes('function group(data)'))+'globalThis.groupSnapshot=group;',box);box.groupSnapshot(snap);w.view.update();const out={sections:w.all().filter(n=>n.classList.contains('repo-toggle')&&!n.parentElement.hidden).map(n=>n.querySelector('.repo-label').textContent),groups:w.all().filter(n=>n.dataset.group).map(n=>n.dataset.group),depths:w.all().filter(n=>n.classList.contains('tree-node')).map(n=>n.dataset.depth)};w.abort.abort();return out;};
  const liveOut=renderPair(warmInput),warmOut=renderPair(cachedData);
  assert.deepEqual(warmOut,liveOut,`warm-cache Workspaces parity for group_by=${groupBy}`);
  assert(!warmOut.groups.includes('a-ext-hidden'));assert(!warmOut.groups.includes('c-main'));
 }
 const homeLive=fixture('settings'),homeWarm=fixture('settings');
 homeLive.ctx.connections=[{id:'pc',name:'Studio',connection:{phase:'ready'},snapshot:warmInput,workspaceView:{hide_sleeping:true}}];
 homeWarm.ctx.connections=[{id:'pc',name:'Studio',connection:{phase:'connecting'},cached:true,snapshot:cachedData,workspaceView:{hide_sleeping:true}}];
 homeLive.view.update();homeWarm.view.update();
 assert.equal(homeWarm.find('.session-summary').textContent,homeLive.find('.session-summary').textContent,'Me desktops workspace count matches warm-cached snapshot');
 homeLive.abort.abort();homeWarm.abort.abort();
 // Missing/mixed orders never yield NaN in Array.prototype.sort across Project, Status, and None modes.
 const origSort=Array.prototype.sort;let sawNaN=false;
 Array.prototype.sort=function(cmp){return origSort.call(this,cmp?(a,b)=>{const r=cmp(a,b);if(Number.isNaN(r))sawNaN=true;return r;}:undefined);};
 try{
  for(const groupBy of [0,1,3]){
   const m=fixture('workspaces',{state:{preferences:{workspaceTree:{},workspaceViews:{'workspace-view':{group_by:groupBy}}}}}),mBox={state:m.state,M:{}};
   vm.createContext(mBox);vm.runInContext(source.split('\n').find(l=>l.includes('function group(data)'))+'globalThis.groupSnapshot=group;',mBox);
   mBox.groupSnapshot({repositories:[{key:'a',label:'Repo'}],workspaces:[{key:'w-z',repo_key:'a',label:'Zeta',status:'todo'},{key:'w-a',repo_key:'a',label:'Alpha',status:'todo'},{key:'w-ranked',repo_key:'a',label:'Ranked',status:'todo',orders:[0,0,0,0]}],sessions:[]});
   m.view.update();
   assert.deepEqual(m.all().filter(n=>n.dataset.group).map(n=>n.dataset.group),['w-ranked','w-a','w-z'],`deterministic order for group_by=${groupBy}`);
   m.abort.abort();
  }
 }finally{Array.prototype.sort=origSort;}
 assert.equal(sawNaN,false,'rank comparator never returns NaN');
 console.log('PASS Orca workspace options: per-device project picker, group by none/status/project, sort, sleeping/automation/CLI filters redraw at once and save on this phone only');
}
async function desktopWorkspaceOrder(){
 const f=fixture('workspaces'),source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8'),sandbox={state:f.state,M:{}};vm.createContext(sandbox);vm.runInContext(source.split('\n').find(l=>l.includes('function group(data)'))+'globalThis.groupSnapshot=group;',sandbox);
 const data={repositories:[{key:'z',label:'Z project',icon:{kind:'emoji',value:'🚀'}},{key:'a',label:'A urgent project'}],workspaces:[{key:'z-main',repo_key:'z',label:'기본',primary:true,depth:0},{key:'z-child',repo_key:'z',label:'Z child',depth:1},{key:'z-alpha',repo_key:'z',label:'Alpha',depth:0},{key:'a-main',repo_key:'a',label:'A main',primary:true,depth:0}],sessions:[{id:1,worktree_key:'z-alpha',state:'Working',lines:['unrelated output']},{id:2,worktree_key:'a-main',state:'Waiting',lines:[]}]};
 sandbox.groupSnapshot(data);f.view.update();const projects=()=>f.all().filter(n=>n.classList.contains('repo-toggle')),shown=()=>f.all().filter(n=>n.dataset.group).map(n=>n.dataset.group);
 assert.deepEqual(projects().map(n=>n.querySelector('.repo-label').textContent),['Z project','A urgent project'],'PC project order beats status and alphabetical sorting');assert.deepEqual(shown(),['z-main','z-child','z-alpha','a-main'],'phone starts with every project expanded');assert(f.find('.workspace-filters').hidden,'filters do not occupy the initial list');assert.equal(projects()[0].querySelector('.repo-summary').textContent,'3 workspaces · 1 terminal');assert.equal(projects()[0].querySelector('.project-icon').textContent,'🚀');
 await projects()[1].action();assert.deepEqual(shown(),['z-main','z-child','z-alpha'],'primary and lineage order are not sorted again on the phone');assert.equal(f.all().find(n=>n.classList.contains('tree-node')&&n.dataset.depth==='1').querySelector('.workspace-row').dataset.group,'z-child');
 const retained=f.all().find(n=>n.dataset.group==='z-child');retained.focus();f.find('.workspace-list').scrollTop=88;data.repositories.reverse();data.workspaces=[data.workspaces[3],data.workspaces[0],data.workspaces[2],data.workspaces[1]];sandbox.groupSnapshot(data);f.view.update();assert.deepEqual(projects().map(n=>n.querySelector('.repo-label').textContent),['A urgent project','Z project']);assert.deepEqual(shown(),['z-main','z-alpha','z-child'],'PC ordering updates on the next snapshot');assert.equal(f.doc.activeElement,retained);assert.equal(f.find('.workspace-list').scrollTop,88);
 f.state.ui.search='project';f.view.update();assert.deepEqual(shown(),['a-main','z-main','z-alpha','z-child'],'search preserves PC order while opening matching projects');f.abort.abort();
 const d=fixture('workspaces');d.state.snapshot.repositories=[{key:'r',label:'krill'}];d.state.snapshot.workspaces=[];d.state.preferences.workspaceTree={'repo:r':true};d.state.groups=new Map(['Krill/worktrees','orca/workspaces'].map((base,i)=>['dup'+i,{key:'dup'+i,repoKey:'r',label:'mobile-ui',branch:'mobile-ui',path:'/local/'+base+'/krill/mobile-ui',sessions:[]}]));d.view.update();const duplicateRows=d.all().filter(n=>n.dataset.group);assert(duplicateRows[0].textContent.includes('worktrees/krill/mobile-ui'));assert(duplicateRows[1].textContent.includes('workspaces/krill/mobile-ui'));d.abort.abort();
 console.log('PASS desktop/mobile order: collapsed A list, project/worktree order, primary/lineage, saved emoji, next snapshot reorder, focus/scroll and filtering');
}
async function projectFilters(){
 const f=fixture('workspaces');const {ga,gb}=groups(f);gb.repoKey='other';f.state.snapshot.repositories.push({key:'other',label:'Second project'});f.state.preferences.workspaceTree.folders=true;const folder={key:'folder',label:'Notes',path:'/tmp/notes',sessions:[]};f.state.groups.set(folder.key,folder);f.state.preferences.workspaceViews={'workspace-view':{repos:['other']}};f.view.update();const shown=()=>f.all().filter(n=>n.dataset.group).map(n=>n.dataset.group);
 assert.deepEqual(shown(),[gb.key,'folder'],'folder sessions stay visible outside the project filter');
 const search=f.all().find(n=>n.tagName==='INPUT'&&n.type==='search');search.value='프로젝트';emit(search,'input');assert.deepEqual(shown(),[ga.key],'search reaches projects outside the filter by name');search.value='';emit(search,'input');
 f.state.preferences.workspaceViews={};f.state.snapshot.repositories=f.state.snapshot.repositories.filter(r=>r.key!=='other');f.view.update();assert.equal(shown().length,3,'a project gone from the snapshot falls back to the folder list');f.abort.abort();
 console.log('PASS project filter keeps folder sessions, search reaches filtered projects, removed repositories');
}
async function createWorktreeSheet(){
 let needsSetup=false;
 const f=fixture('workspaces',{rpc:req=>{if(req.action==='create_workspace'){if(needsSetup&&!req.setup_hash&&!req.skip_setup)return {needs_setup:true,script:'npm ci',hash:'a'.repeat(64)};f.state.groups.set('wt-new',{key:'wt-new',repoKey:req.repository,label:req.name,path:'/repo/'+req.name,sessions:[{id:77}]});return {workspace:'wt-new',session:77,branch:req.name,path:'/repo/'+req.name};}return {};}});
 f.state.snapshot.capabilities={worktree_create:true};
 f.state.snapshot.agents=[{id:'claude',name:'Claude Code'},{id:'codex',name:'Codex'}];
 f.state.snapshot.repositories=[{key:'krill',label:'krill',path:'/Users/macmini/Documents/wProject/krill',remote:'craftowen/krill',badge_color:'#3b82f6'},{key:'orca',label:'orca',path:'/Users/macmini/Documents/wProject/orca',remote:'lovecast/orca',icon:{kind:'emoji',value:'🐋'}},{key:'old',label:'old',removed:true}];
 f.state.groups.set('g-1',{key:'g-1',repoKey:'krill',label:'otter',branch:'main',path:'/Users/macmini/Documents/wProject/krill',sessions:[]});
 f.state.groups.set('g-2',{key:'g-2',repoKey:'krill',label:'feat',branch:'feat/mobile-ui',path:'/Users/macmini/Krill/worktrees/krill/feat',sessions:[]});
 f.state.preferences.workspaceViews={'workspace-view':{repos:['orca']}};f.view.update();
 const fab=f.byText('New workspace');assert(fab.classList.contains('new-workspace-fab'));assert.equal(fab.disabled,false);
 f.state.connection.phase='disconnected';f.view.update();assert.equal(fab.disabled,true);f.state.connection.phase='ready';f.view.update();assert.equal(fab.disabled,false);
 await fab.action();assert.equal(f.sheets.at(-1)[0],'Create worktree');
 const body=f.ctx.el('div');f.sheets.at(-1)[3](body);const form=body.querySelector('.worktree-composer');assert(form);
 const projectBtn=form.querySelector('.worktree-project-button'),targetBtn=form.querySelector('.worktree-target-button'),agentBtn=form.querySelector('.worktree-agent-button'),nameInput=form.querySelector('.worktree-name-input'),advBtn=form.querySelector('.worktree-advanced-toggle'),advBody=form.querySelector('.worktree-advanced-body');
 assert.equal(projectBtn.querySelector('.worktree-field-title').textContent,'orca','single filtered project is preselected');
 assert.equal(projectBtn.querySelector('.worktree-field-detail').textContent,'lovecast/orca');
 assert.equal(projectBtn.querySelector('.project-choice-dot').dataset.emoji,'🐋');
 await projectBtn.action();const projectList=form.querySelector('.project-picker-list');assert.equal(projectList.hidden,false);assert.deepEqual(projectList.children.map(n=>n.dataset.repoKey),['krill','orca'],'removed repositories are excluded');
 await projectList.children[0].action();assert.equal(projectList.hidden,true);
 assert.equal(projectBtn.querySelector('.worktree-field-title').textContent,'krill');
 assert.equal(projectBtn.querySelector('.worktree-field-detail').textContent,'craftowen/krill');
 assert.equal(targetBtn.querySelector('.worktree-field-title').textContent,'Test desktop');
 assert.equal(targetBtn.querySelector('.worktree-field-detail').textContent,'/Users/macmini/Documents/wProject/krill');
 assert.equal(agentBtn.querySelector('.worktree-agent-name').textContent,'Claude');
 assert.equal(nameInput.placeholder,'Type a name or search a source');
 // Submitting with blank name auto-generates a unique creature name (skipping 'otter' which is already in state.groups).
 emit(form,'submit');await f.flush();
 assert.deepEqual(JSON.parse(JSON.stringify(f.calls.at(-1))),{action:'create_workspace',repository:'krill',name:'falcon',agent:'claude'});
 assert.equal(f.routes.at(-1),'/host/local/group/wt-new/s/77');
 // Reopen, pick branch from source suggestions, switch agent to Codex, and test setup script review.
 f.state.preferences.workspaceViews={};f.view.update();await fab.action();const body2=f.ctx.el('div');f.sheets.at(-1)[3](body2);const form2=body2.querySelector('.worktree-composer');
 const name2=form2.querySelector('.worktree-name-input'),sources2=form2.querySelector('.worktree-source-list'),agentBtn2=form2.querySelector('.worktree-agent-button'),agentList2=form2.querySelector('.agent-picker-list'),advBody2=form2.querySelector('.worktree-advanced-body');
 emit(name2,'focus');assert.equal(sources2.hidden,false);assert.deepEqual(sources2.children.map(n=>n.querySelector('.worktree-source-branch').textContent),['main','feat/mobile-ui']);
 await sources2.children[1].action();assert.equal(advBody2.hidden,false);assert.equal(advBody2.querySelector('input').value,'feat/mobile-ui');
 name2.value='custom-wt';emit(name2,'input');
 await agentBtn2.action();assert.equal(agentList2.hidden,false);await agentList2.children.find(n=>n.dataset.agentId==='codex').action();assert.equal(agentBtn2.querySelector('.worktree-agent-name').textContent,'Codex');
 needsSetup=true;emit(form2,'submit');await f.flush();
 assert.deepEqual(JSON.parse(JSON.stringify(f.calls.at(-1))),{action:'create_workspace',repository:'krill',name:'custom-wt',base:'feat/mobile-ui',agent:'codex'});
 assert.equal(f.sheets.at(-1)[0],'Run setup script?');
 await f.sheets.at(-1)[1][0].action();await f.flush();
 assert.deepEqual(JSON.parse(JSON.stringify(f.calls.at(-1))),{action:'create_workspace',repository:'krill',name:'custom-wt',base:'feat/mobile-ui',agent:'codex',setup_hash:'a'.repeat(64)});
 f.abort.abort();
 // Multi-desktop "Run on" omits offline/cached/unsupported desktops and guards mid-open disconnect; app.js newWorkspace & pendingHostAction lifecycle.
 const md=fixture('workspaces');
 md.state.snapshot.capabilities={worktree_create:true};
 md.state.snapshot.repositories=[{key:'krill',label:'krill',path:'/krill'}];
 const connA={id:'a',name:'Studio Mac',active:true,endpoint:'Relay · aaaa',connection:{phase:'ready'},snapshot:{capabilities:{worktree_create:true},repositories:[{key:'krill',label:'krill',path:'/krill'}]}},
  connB={id:'b',name:'Linux Box',active:false,endpoint:'Relay · bbbb',connection:{phase:'ready'},snapshot:{capabilities:{worktree_create:true},repositories:[{key:'krill',label:'krill',path:'/krill'}]}},
  connOff={id:'off',name:'Sleeping MacBook',active:false,endpoint:'Relay · cccc',connection:{phase:'disconnected'},snapshot:{capabilities:{worktree_create:true}}},
  connCached={id:'cached',name:'Cached Mac',active:false,endpoint:'Relay · dddd',cached:true,connection:{phase:'ready'},snapshot:{capabilities:{worktree_create:true}}},
  connOld={id:'old',name:'Legacy Mac',active:false,endpoint:'Relay · eeee',connection:{phase:'ready'},snapshot:{capabilities:{worktree_create:false},repositories:[{key:'krill'}]}};
 md.ctx.connections=[connA,connB,connOff,connCached,connOld];
 md.ctx.hosts=[connA,connB,connOff,connCached,connOld];
 md.view.update();
 await md.byText('New workspace').action();
 const mdBody=md.ctx.el('div');md.sheets.at(-1)[3](mdBody);const mdForm=mdBody.querySelector('.worktree-composer'),mdTargetBtn=mdForm.querySelector('.worktree-target-button'),mdTargetList=mdForm.querySelector('.target-picker-list');
 await mdTargetBtn.action();
 assert.deepEqual(mdTargetList.children.map(n=>n.querySelector('.worktree-field-title').textContent),['Studio Mac','Linux Box'],'Run on lists only ready non-cached desktops supporting worktree_create');
 connB.connection.phase='disconnected';
 await mdTargetList.children[1].action();
 assert.equal(md.ctx.selected,undefined,'disconnecting while Run on is open prevents host switch');
 connB.connection.phase='ready';
 await mdTargetBtn.action();
 let reopenedRepo=null;md.ctx.newWorkspace=repoKey=>{reopenedRepo=repoKey;};
 await mdTargetList.children[1].action();
 assert.equal(md.ctx.selected,'b');assert.equal(reopenedRepo,'krill');
 md.abort.abort();
 // Exercise real app.js newWorkspace(), selectHost(id, after), go() and hashchange cancellation.
 {
  const appSource=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8'),lines=appSource.split('\n'),pick=m=>lines.find(l=>l.trim().startsWith(m));
  const selectIdx=lines.findIndex(l=>l.trim().startsWith('function selectHost(')),hashLine=lines.find(l=>l.includes("globalThis.addEventListener('hashchange'")),hashHandler=hashLine.slice(hashLine.indexOf("globalThis.addEventListener('hashchange'"));
  let activeId='off',composers=[],sheetsOpen=[],renders=0,reloads=0;
  const profiles=[{id:'off',name:'Sleeping MacBook',endpoint:'Relay · off',get active(){return activeId==='off';}},{id:'studio',name:'Studio Mac',endpoint:'Relay · studio',get active(){return activeId==='studio';}},{id:'linux',name:'Linux Box',endpoint:'Relay · linux',get active(){return activeId==='linux';}}];
  const entries=new Map([
   ['off',{id:'off',transport:{ready:false,setActive(){}},connection:{phase:'disconnected',message:'Not connected'},snapshot:{capabilities:{worktree_create:true}},cached:false,endpoint:{credentials:{device_id:'d1'}} }],
   ['studio',{id:'studio',transport:{ready:true,setActive(){}},connection:{phase:'ready',message:'Connected'},snapshot:{capabilities:{worktree_create:true},repositories:[{key:'r1'}]},cached:false,endpoint:{credentials:{device_id:'d2'}} }],
   ['linux',{id:'linux',transport:{ready:false,setActive(){}},connection:{phase:'connecting',message:'Connecting…'},snapshot:{capabilities:{worktree_create:true},repositories:[{key:'r1'}]},cached:false,endpoint:{credentials:{device_id:'d3'}} }]
  ]);
  const win=new EventTarget(),historyEntries=['#/'];let histIdx=0;
  const loc={pathname:'/',search:'',reload(){reloads++;}};
  Object.defineProperty(loc,'hash',{get:()=>historyEntries[histIdx],set:v=>{if(v===historyEntries[histIdx])return;historyEntries.splice(histIdx+1,Infinity,v);histIdx++;queueMicrotask(()=>win.dispatchEvent(new Event('hashchange')));}});
  const box={
   location:loc,history:{replaceState:(_a,_b,url)=>{historyEntries[histIdx]=url.slice(url.indexOf('#'));},back:()=>{histIdx--;queueMicrotask(()=>win.dispatchEvent(new Event('hashchange')));}},
   addEventListener:win.addEventListener.bind(win),URLSearchParams,
   endpoint:{profiles:()=>profiles,selectHost:id=>{activeId=id;return {device_id:'d-'+id};}},
   pool:{sync(){},get:id=>entries.get(id),owns:()=>true},
   state:{connection:entries.get('off').connection,cached:false,logs:[],preferences:{},groups:new Map(),sessions:new Map()},
   transport:entries.get('off').transport,landing:false,activeRouteScreen:'home',
   banner:{replaceChildren(){},append(){},hidden:true,className:''},notice:{hidden:true,textContent:''},dialog:{open:false,close(){this.open=false;}},
   hideNotice(){},update(){},render(){renders++;box.activeRouteScreen=box.parseRoute().screen;},clearHost(){},
   activateEntry:entry=>{activeId=entry.id;box.transport=entry.transport;box.state.connection=entry.connection;box.state.cached=entry.cached;box.state.snapshot=entry.snapshot;},
   workspaceComposer:repo=>composers.push([activeId,repo]),
   sheet:(title,items)=>sheetsOpen.push({title,items}),closeSheet(){box.dialog.open=false;},scanner:null,
   el:()=>({append(){}}),button:()=>({}),t:s=>s,M:{recentSession:{list:()=>[]}}
  };
  box.globalThis=box;
  vm.createContext(box);
  vm.runInContext([
   pick('let routeStack='),pick('const hostRoute='),pick('const sessionPath='),pick('const go='),pick('function back('),pick('const pairingLink='),
   pick('function parseRoute('),pick('function showConnection('),lines[selectIdx],lines[selectIdx+1],pick('function getConnections('),
   pick('const canCreateWorkspace='),pick('function newWorkspace('),hashHandler
  ].join('\n')+'\nglobalThis.parseRoute=parseRoute;globalThis.newWorkspace=newWorkspace;globalThis.selectHost=selectHost;globalThis.go=go;globalThis.back=back;globalThis.showConnection=showConnection;',box);
  // 1) Home with 1 offline active desktop + 1 online ready desktop -> skips "Choose desktop" and selects Studio Mac directly!
  box.newWorkspace('r1');
  assert.equal(sheetsOpen.length,0,'single ready desktop skips offline desktop chooser');
  assert.equal(activeId,'studio');
  assert.deepEqual(composers,[['studio','r1']]);
  // 2) Home with 2 ready desktops + 1 offline -> "Choose desktop" lists only the 2 ready desktops.
  entries.get('linux').transport.ready=true;entries.get('linux').connection={phase:'ready',message:'Connected'};
  box.go('/',true);box.newWorkspace('r1');
  assert.equal(sheetsOpen.length,1);
  assert.deepEqual(sheetsOpen[0].items.map(i=>i.label),['Studio Mac · Relay · studio','Linux Box · Relay · linux']);
  // 3) pendingHostAction runs when staying on target host, but cancels on go() or Back (hashchange).
  entries.get('linux').transport.ready=false;entries.get('linux').connection={phase:'connecting',message:'Connecting…'};
  let fired=0;
  box.go('/',true);box.selectHost('linux',()=>{fired++;});
  entries.get('linux').transport.ready=true;entries.get('linux').connection={phase:'ready',message:'Connected'};
  box.showConnection(entries.get('linux').connection);
  assert.equal(fired,1,'valid selectHost callback runs once target becomes ready');
  // Arm on connecting active host from Home, let initial #/host/linux hashchange settle, then navigate away via go('/settings').
  entries.get('linux').transport.ready=false;entries.get('linux').connection={phase:'connecting',message:'Connecting…'};
  box.go('/',true);box.selectHost('linux',()=>{fired++;});
  await Promise.resolve();await Promise.resolve();
  box.go('/settings');
  await Promise.resolve();await Promise.resolve();
  entries.get('linux').transport.ready=true;entries.get('linux').connection={phase:'ready',message:'Connected'};
  box.showConnection(entries.get('linux').connection);
  assert.equal(fired,1,'go() cancels stale pendingHostAction before delayed ready');
  // Arm again from Home, let initial #/host/linux hashchange settle, then press Back to Home (#/).
  entries.get('linux').transport.ready=false;entries.get('linux').connection={phase:'connecting',message:'Connecting…'};
  box.go('/',true);box.selectHost('linux',()=>{fired++;});
  await Promise.resolve();await Promise.resolve();
  box.back();
  await Promise.resolve();await Promise.resolve();
  assert.equal(loc.hash,'#/');
  entries.get('linux').transport.ready=true;entries.get('linux').connection={phase:'ready',message:'Connected'};
  box.showConnection(entries.get('linux').connection);
  assert.equal(fired,1,'Back hashchange cancels stale pendingHostAction');
  assert.equal(reloads,0);
 }
 console.log('PASS Orca Create worktree sheet: FAB, project/remote picker, Run on target, branch source suggestions, agent picker, auto-name and setup review');
}
async function home(){
 // 1) Unpaired Home shows pairing hero (icon.svg + Scan QR + Paste link)
 const f=fixture('home');assert.equal(f.find('.krill-logo').src,'icon.svg');assert(!f.root.textContent.includes('Welcome back'));assert(f.root.textContent.includes('Connect your desktop'));
 // 2) Paired + bots capable Home opens Bots home directly (no desktop gateway) + recent sessions (max 3) + View all -> /projects
 const first={id:'one',active:true,name:'First PC',endpoint:'Relay · aaaa',connection:{phase:'ready'},snapshot:{capabilities:{bots:true,worktree_create:true},repositories:[{key:'r1',label:'krill'}],workspaces:[{key:'same',repo_key:'r1',label:'main'}],sessions:[{id:7,title:'First task',worktree_key:'same',state:'Working'}],orchestration:{}}},second={id:'two',name:'Second PC',endpoint:'Relay · bbbb',connection:{phase:'ready'},snapshot:{capabilities:{bots:true,worktree_create:true},repositories:[{key:'r2',label:'docs'}],workspaces:[{key:'same',repo_key:'r2',label:'main'}],sessions:[{id:7,title:'Second task',worktree_key:'same',state:'Waiting'}],orchestration:{}}};
 f.ctx.connections=[first,second];f.view.update();await f.flush();
 assert(f.find('.bots-list'),'paired bots-capable home mounts Bots home directly');
 assert(!f.find('.home-desktops'),'no desktop gateway on Bots home');
 const banner=f.find('.todo-summary-banner');assert(banner,'Waiting session shows 1-line summary banner');
 const rows=f.find('.recent-session-list').children;assert.equal(rows[0].dataset.host,'two','Waiting session sorts ahead of Working');assert.equal(rows[1].dataset.host,'one');
 await rows[0].action();assert.equal(f.ctx.opened.id,'two');assert.equal(f.ctx.opened.locator.id,7);assert.equal(f.ctx.opened.locator.groupKey,'same');
 await f.byText('View all').action();assert.equal(f.routes.at(-1),'/projects');
 f.abort.abort();
 // 3) Paired + legacy daemons (all lack capabilities.bots) safely fall back to Projects (workspaces)
 const legacy=fixture('home');
 legacy.ctx.connections=[{id:'old',active:true,name:'Old Mac',connection:{phase:'ready'},snapshot:{capabilities:{},repositories:[{key:'r1',label:'legacy-repo'}],workspaces:[{key:'w1',repo_key:'r1',label:'main'}],sessions:[]}}];
 legacy.view.update();
 assert(legacy.find('.workspace-list'),'legacy daemons without bots capability fall back to Projects');
 legacy.abort.abort();
 // 4) Multi-host Projects view aggregates both desktops with quiet device badges and zero reconnect
 const proj=fixture('workspaces');proj.ctx.connections=[first,second];proj.view.update();
 const repoLabels=proj.all().filter(n=>n.classList.contains('repo-label')).map(n=>n.textContent);
 assert.deepEqual(repoLabels,['krill','docs']);
 const devBadges=proj.all().filter(n=>n.classList.contains('device-badge')&&!n.hidden).map(n=>n.textContent);
 assert.deepEqual(devBadges,['First PC','Second PC'],'multi-host projects display quiet device badges');
 proj.abort.abort();
 // 5) Unified Me hub (settings) hosts Connected Desktops (rename/reconnect/remove/pair/paste) + Usage + Settings
 const me=fixture('settings');me.ctx.connections=[first,second];me.view.update();await me.flush();
 assert.equal(me.find('.home-desktops').children.length,2);assert(me.root.textContent.includes('2/2'));
 assert(me.find('.home-desktop-group').contains(me.byText('Pair a desktop')));
 assert(me.find('.home-desktop-group').contains(me.byText('Paste pairing link')));
 const cards=me.find('.home-desktops').children,firstButton=cards[0].querySelector('.host-main'),other=cards[1].querySelector('.host-main');
 first.name='Renamed PC';me.view.update();assert(firstButton.textContent.includes('Renamed PC'));
 second.connection.phase='disconnected';me.view.update();assert(me.root.textContent.includes('1/2'));assert(other.textContent.includes('Not connected'));assert.equal(cards[1].querySelector('.status-dot').className,'status-dot unknown');
 second.connection.phase='connecting';me.view.update();assert.equal(cards[1].querySelector('.status-dot').className,'status-dot working');
 second.connection.phase='denied';me.view.update();assert.equal(cards[1].querySelector('.status-dot').className,'status-dot blocked');
 other._longPress();assert.deepEqual(Array.from(me.sheets.at(-1)[1],a=>a.label),['Rename desktop','Reconnect','Remove from this phone']);assert.equal(me.sheets.at(-1)[1].at(-1).danger,true);
 firstButton._longPress();assert.deepEqual(Array.from(me.sheets.at(-1)[1],a=>a.label),['Rename desktop','Remove from this phone']);assert.equal(me.sheets.at(-1)[1].at(-1).danger,true);
 cards[1].querySelectorAll('button')[1].action();await me.sheets.at(-1)[1].find(a=>a.label==='Reconnect').action();assert.equal(me.ctx.reconnected,'two');await me.sheets.at(-1)[1].find(a=>a.label==='Rename desktop').action();assert.equal(me.ctx.renamed,'two');await me.byText('Pair a desktop').action();assert.equal(me.ctx.scans,1);
 me.abort.abort();
 console.log('PASS Variant B Home/Projects/Me: pairing hero, Bots home with banner+recents, legacy fallback to Projects, multi-host Projects badges, Me hub desktops/usage/settings');
}
async function homeResumeAndLogins(){
 const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/home.js'),'utf8'),storage=new Map(),context=vm.createContext({localStorage:{getItem:k=>storage.get(k),setItem:(k,v)=>storage.set(k,v),removeItem:k=>storage.delete(k)}});vm.runInContext(source,context);const R=context.KrillMobile.recentSession;
 assert.equal(R.read('phone-a'),null);R.write('phone-a',{id:7,title:'do not persist terminal text'},{key:'cwd:/work'});R.write('phone-b',{id:7},{key:'second-host'});R.write('phone-a',{id:8},{key:'another'});assert.equal(R.read('phone-a').id,8);assert.equal(R.list('phone-a').length,2);assert.equal(R.read('phone-b').groupKey,'second-host');assert(![...storage.values()][0].includes('terminal text'));R.write('phone-a',{id:9,exited:true},{key:'g'});assert.equal(R.read('phone-a').id,8);R.clear();assert.equal(R.read('phone-a'),null);
 for(const raw of ['bad json','null','{"device":"phone-a","id":-1,"groupKey":"g"}','{"device":"phone-a","id":7}']){storage.set('krill-mobile-last-session-v1',raw);assert.equal(R.read('phone-a'),null);}context.localStorage.getItem=()=>{throw Error('storage disabled');};assert.equal(R.read('phone-a'),null);context.localStorage.setItem=()=>{throw Error('quota');};assert.equal(R.write('phone-a',{id:9},{key:'g'}).id,9);
 console.log('PASS recent history stores only scoped locators, multiple sessions per PC, exited rejection and malformed/full storage');
}
async function git(){let summaryAttempt=0;const patchA=deferred(),patchB=deferred();let nextSummary={branch:'main',files:['a.rs','b.rs'],added:2,deleted:1};const f=fixture('git',{route:{sessionId:7},rpc:req=>{if(req.path===null){if(++summaryAttempt===1)throw Error('Git unavailable');return nextSummary;}return req.path==='a.rs'?patchA.promise:patchB.promise;}});assert(f.root.textContent.includes('Loading Git changes…'));await f.flush();assert(f.root.textContent.includes('Could not load Git changes: Git unavailable'));assert.equal(f.byText('Retry').hidden,false);await f.byText('Retry').action();assert(f.root.textContent.includes('Updated:'));assert.equal(f.find('.changed-files').children.length,2);const a=f.byText('a.rs'),b=f.byText('b.rs');const loadA=a.action();assert.equal(a.nextSibling,f.find('.git-selected'));assert.equal(f.find('.git-selected').nextSibling,f.find('.git-patch'));assert.equal(f.find('.git-patch').nextSibling,b);const loadB=b.action();assert.equal(b.nextSibling,f.find('.git-selected'));assert.equal(f.find('.git-selected').nextSibling,f.find('.git-patch'));assert(f.root.textContent.includes('Selected file: b.rs'));patchB.resolve({patch:'+current B'});await loadB;patchA.resolve({patch:'+obsolete A'});await loadA;assert(f.find('.git-patch').textContent.includes('current B'));assert(!f.find('.git-patch').textContent.includes('obsolete A'));assert.equal(a.getAttribute('aria-pressed'),'false');assert.equal(b.getAttribute('aria-pressed'),'true');assert(f.find('.git-selected').scrolledIntoView);assert(/Updated: \d{2}:\d{2}:\d{2}/.test(f.find('.git-status').textContent));
 f.ctx.rpc=async req=>{f.calls.push(req);if(req.path!==null)throw Error('Diff unavailable');return nextSummary;};await a.action();assert.equal(a.nextSibling,f.find('.git-selected'));assert.equal(f.find('.git-selected').nextSibling,f.find('.git-patch'));assert.equal(f.find('.git-patch').nextSibling,b);assert(f.root.textContent.includes('Could not load diff: Diff unavailable'));assert.equal(f.byText('Retry').hidden,false);assert(!f.find('.git-patch').textContent.includes('current B'));f.ctx.rpc=async req=>{f.calls.push(req);return req.path===null?nextSummary:{patch:'@@ reloaded\n+fixed'};};await f.byText('Retry').action();assert(f.find('.git-patch').textContent.includes('+fixed'));assert.equal(f.byText('Retry').hidden,true);
 a.focus();f.find('.detail-list').scrollTop=93;await f.byText('Refresh').action();await f.flush();assert.equal(f.byText('a.rs'),a);assert.equal(a.nextSibling,f.find('.git-selected'));assert.equal(f.find('.git-selected').nextSibling,f.find('.git-patch'));assert.equal(f.find('.git-patch').nextSibling,b);assert.equal(f.doc.activeElement,a);assert.equal(f.find('.detail-list').scrollTop,93);assert(f.root.textContent.includes('Selected file: a.rs'));nextSummary={branch:'main',files:[]};await f.byText('Refresh').action();assert.equal(f.find('.changed-files').textContent,'No changes');assert.equal(f.find('.git-patch').textContent,'No changes');assert.equal(f.find('.git-selected').textContent,'Select a changed file');f.state.connection.phase='disconnected';f.view.update();assert.equal(f.byText('Refresh').disabled,true);f.abort.abort();
 const deferredSummary=deferred(),g=fixture('git',{route:{sessionId:9},rpc:()=>deferredSummary.promise});const initial=g.root.textContent;g.abort.abort();deferredSummary.resolve({files:['late'],branch:'late'});await g.flush();assert.equal(g.root.textContent,initial);
 console.log('PASS SC-A07/A08: Git inline load/error/Retry/Refresh, current selection, stale response suppression, no stale patch, focus/scroll, abort');}
async function accounts(){const data={accounts:[{id:'work',provider:'Claude',label:'Work',email:'아주긴계정'.repeat(25)+'@example.test',profile_dir:'/work',active:false,stale:false,usage:{five_hour:{percent:83,resets_at:String(Math.floor(Date.now()/1000)+7200)},seven_day:null}}],statuses:[{account_id:'work',status:'needs_login',detail:'Run claude login',updated_at:Math.floor(Date.now()/1000)}],connections:{supported:true,accounts:[{account_id:'work',status:'needs_login'}]}};const f=fixture('accounts',{rpc:()=>data});await f.flush();const row=f.find('.account-row'),description=f.all().find(n=>n.id===row.getAttribute('aria-describedby'));assert(row.getAttribute('aria-label').startsWith('Switch account:'));assert(description.textContent.includes('Sign-in required'));assert(/83%.*[12]h/.test(description.textContent));assert(description.textContent.includes('Run claude login'));assert(description.textContent.includes('Unknown'));row.focus();f.find('.account-list').scrollTop=73;await f.byText('Refresh accounts').action();assert.equal(f.find('.account-row'),row);assert.equal(f.doc.activeElement,row);assert.equal(f.find('.account-list').scrollTop,73);f.state.connection.phase='disconnected';f.view.update();assert(row.disabled);assert(f.root.textContent.includes('Reconnect to switch accounts.'));assert.equal(f.headers[0].fallback,'/');f.abort.abort();
 const g=fixture('accounts',{rpc:()=>{throw Error('Load failed');}});await g.flush();assert(g.root.textContent.includes('Load failed'));assert(!g.byText('Retry').hidden);g.abort.abort();const offline=fixture('accounts',{state:{connection:{phase:'disconnected'}},rpc:()=>data});await offline.flush();assert.equal(offline.calls.length,0);offline.state.connection.phase='ready';offline.view.update();await offline.flush();assert.equal(offline.calls.length,1);offline.abort.abort();
 // The account picker uses live connections, including when only one alternative is online.
 const picker=fixture('accounts',{state:{snapshot:null,connection:{phase:'disconnected'}},rpc:()=>data}),active={id:'active',name:'Offline active',active:true,connection:{phase:'disconnected'}},live={id:'live',name:'Online desktop',connection:{phase:'ready'}},cached={id:'cached',name:'Cached desktop',connection:{phase:'ready'},cached:true};
 picker.ctx.connections=[active,live,cached,...['idle','connecting','disconnected'].map(phase=>({id:phase,name:phase,connection:{phase}}))];picker.view.update();const chooser=picker.byText('Choose desktop');assert(!chooser.hidden,'a single online alternative remains reachable from an offline account page');assert.equal(picker.calls.length,0);
 chooser.action();let choices=picker.sheets.at(-1)[1];assert.deepEqual(Array.from(choices,h=>h.label),['Online desktop'],'offline, reconnecting and cached hosts are omitted');
 live.connection.phase='disconnected';await choices[0].action();assert.equal(picker.ctx.selected,undefined,'an already-open picker cannot select a host that disconnected');assert(chooser.hidden);live.connection.phase='ready';picker.view.update();assert(!chooser.hidden);chooser.action();await picker.sheets.at(-1)[1][0].action();assert.equal(picker.ctx.selected,'live');assert.equal(picker.routes.at(-1),'/accounts');
 picker.state.connection.phase='ready';active.connection.phase='ready';picker.view.update();chooser.action();assert.equal(picker.sheets.at(-1)[1].length,2);live.connection.phase='connecting';picker.view.update();assert(chooser.hidden,'other-host changes update the picker while the active connection stays ready');live.connection.phase='ready';picker.view.update();assert(!chooser.hidden);chooser.action();choices=picker.sheets.at(-1)[1];picker.abort.abort();await choices[1].action();assert.equal(picker.routes.length,1,'aborted screens cannot switch hosts');
 const A=f.M.accounts,now=1000000;assert.equal(A.usageWindow(null,now).percent,null);assert.equal(A.usageWindow({percent:88,resets_at:String(now-1)},now).text,'—');assert.equal(A.usageWindow({percent:40,resets_at:new Date((now+90)*1000).toISOString()},now).reset,'1m');assert.equal(A.countdown(2*86400+3600),'2d');assert.equal(A.countdown(5*3600,()=>{throw Error('duration must not translate');}),'5h');assert.equal(A.countdown(7*86400),'7d');assert.equal(A.usageWindow({percent:NaN},now).text,'—');
 let refreshes=0,handoffState={accounts:[{id:'agy-2',provider:'Antigravity',label:'Agy 2',email:'agy2@example.test',profile_dir:'/agy2',active:false,stale:false,usage:{five_hour:null,seven_day:null}}],statuses:[],connections:{supported:true,accounts:[{account_id:'agy-2',status:'connected'}]},details:{handoffs:[]}};
 const h=fixture('accounts',{refresh:()=>{refreshes++;},rpc:req=>{if(req.action==='switch_account'){handoffState={...handoffState,accounts:[{...handoffState.accounts[0],active:true}],details:{handoffs:[{from:7,to:null,account_id:'agy-2',error:null}]}};return handoffState;}return handoffState;}});await h.flush();
 await h.find('.account-row').action();await h.flush();assert.equal(refreshes,1);assert(h.find('.account-trailing').querySelector('.spinner'),'pending session handoff shows spinner');
 handoffState={...handoffState,details:{handoffs:[{from:7,to:null,account_id:'agy-2',error:'Session 7: could not resume with selected account (os error 2)'}]}};
 for(const [id,t] of [...h.timers])if(!t.interval){h.timers.delete(id);t.fn();}await h.flush();
 assert.equal(refreshes,2,'settling handoff refreshes mobile snapshot');
 const handoffErr=h.find('.account-error');
 assert(handoffErr,'settling handoff renders .account-error');
 assert.equal(handoffErr.textContent,"Couldn't switch a running session to this account. Kept the original session.");
 assert(!h.root.textContent.includes('Session 7'));
 assert(!h.root.textContent.includes('os error'));
 assert(!handoffErr.textContent.includes('('));
 h.abort.abort();
 const locale=vm.createContext({navigator:{language:'ko'}});vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/i18n.js'),'utf8'),locale);const I=locale.KrillMobile.i18n;
 const diagAccounts={
  accounts:[
   {id:'claude-401',provider:'Claude',label:'Claude 401',email:'claude@example.test',profile_dir:'/c401',active:false,stale:false,usage:{five_hour:null,seven_day:null}},
   {id:'agy-2',provider:'Antigravity',label:'Agy 2',email:'agy2@example.test',profile_dir:'/agy2',active:false,stale:false,usage:{five_hour:null,seven_day:null}},
   {id:'opencode-default',provider:'OpenCode',label:'OpenCode default',host_default:true,email:null,profile_dir:null,active:true,stale:false,usage:{five_hour:null,seven_day:null}},
  ],
  statuses:[
   {account_id:'claude-401',status:'needs_login',detail:'Usage API rejected the sign-in (HTTP 401)'},
   {account_id:'opencode-default',status:'error',detail:'Usage API denied access (HTTP 403)'},
  ],
  connections:{supported:true,accounts:[
   {account_id:'claude-401',status:'needs_login'},
   {account_id:'agy-2',status:'connected'},
   {account_id:'opencode-default',status:'connected'},
  ]},
  details:{handoffs:[{from:41205,to:null,account_id:'agy-2',error:'Session 41205: could not resume with selected account'}]},
 };
 for(const [lang,tr] of [['en',s=>s],['ko',I.t]]){
  const d=fixture('accounts',{t:tr,rpc:()=>diagAccounts});await d.flush();
  const rows=d.root.querySelectorAll('.account-row');assert.equal(rows.length,3);
  const [cRow,aRow,oRow]=rows;
  assert.equal(cRow.querySelector('.account-error')?.textContent,tr('Sign in to this account again to check usage.'),lang+' 401 error');
  assert.equal(aRow.querySelector('.account-error')?.textContent,tr("Couldn't switch a running session to this account. Kept the original session."),lang+' handoff 41205');
  assert.equal(oRow.querySelector('.account-error'),null,lang+' OpenCode host_default 403 must not show .account-error');
  assert(oRow.querySelector('.account-meta')?.textContent.includes(tr("Usage details aren't available for this account.")),lang+' OpenCode host_default 403 shows muted note');
  assert(!oRow.textContent.includes(tr('Usage error')),lang+' OpenCode host_default 403 suppresses Usage error badge');
  for(const raw of ['HTTP 401','HTTP 403','Usage API','Session 41205','41205'])assert(!d.root.textContent.includes(raw),lang+' hides '+raw);
  if(lang==='ko'){
   assert.notEqual(cRow.querySelector('.account-error').textContent,'Sign in to this account again to check usage.');
   assert.notEqual(aRow.querySelector('.account-error').textContent,"Couldn't switch a running session to this account. Kept the original session.");
  }
  d.abort.abort();
 }
 assert.equal(A.formatUsageDetail({detail:'Usage API rate limited (HTTP 429) — last successful value'},{provider:'Claude',host_default:false},I.t).error,`${I.t('Usage check is temporarily rate-limited. Try again shortly.')} (${I.t('showing last known usage')})`);
 assert.equal(A.formatHandoffDetail('Replacement session 99 was retained for manual selection',I.t),I.t('Kept the new session for manual selection after the original ended. Held input was not sent.'));
 console.log('PASS SC-A04/A05/A12: Accounts stable focus/scroll, action name and associated complete login/error/reset context, inline Retry');}
async function quickCommandSettings(){
 const f=fixture('settings',{route:{subpage:'terminal'},rpc:req=>({quick_commands:req.commands?.map(c=>({...c,name:c.name.toUpperCase()}))})});
 const manager=f.find('.quick-actions-settings'),find=label=>manager.descendants().find(n=>n.tagName==='BUTTON'&&n.textContent===label);
 assert(manager.textContent.includes('No quick actions yet.'));const add=find('+ Add');add.focus();for(let i=0;i<20;i++)f.view.update();assert.equal(find('+ Add'),add);assert.equal(f.doc.activeElement,add);
 await find('+ Add quick action').action();let name=manager.querySelector('input'),text=manager.querySelector('textarea');name.value='ship';text.value='/ship-dev ';text.focus();text.selectionStart=2;text.selectionEnd=5;emit(text,'compositionstart');const form=manager.querySelector('form');for(let i=0;i<30;i++){f.state.snapshot.accounts=[{usage:i}];f.view.update();}assert.equal(manager.querySelector('form'),form);assert.equal(manager.querySelector('input'),name);assert.equal(manager.querySelector('textarea'),text);assert.equal(f.doc.activeElement,text);assert.equal(text.selectionStart,2);assert.equal(text.selectionEnd,5);assert.equal(text.value,'/ship-dev ');assert.equal(name.value,'ship');assert.equal(f.calls.length,0,'snapshots never save a manual draft');emit(text,'compositionend');await find('Save').action();assert.equal(f.calls.at(-1).action,'save_quick_commands');assert.equal(f.state.snapshot.quick_commands[0].name,'SHIP');assert.equal(f.state.preferences.quickCommands[0].text,'/ship-dev ');assert(manager.textContent.includes('SHIP'));
 await find('Edit').action();name=manager.querySelector('input');text=manager.querySelector('textarea');name.value='review';text.value='Review this\nexplain tradeoffs';await find('Save').action();assert.equal(f.state.preferences.quickCommands[0].text,'Review this\nexplain tradeoffs');await find('Delete').action({stopPropagation(){}});assert.equal(f.state.preferences.quickCommands.length,0);assert.equal(f.calls.at(-1).commands.length,0);
 const locale=vm.createContext({navigator:{language:'ko'}});vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/i18n.js'),'utf8'),locale);const I=locale.KrillMobile.i18n;
 for(const key of ['Quick actions','Add','Add quick action','Name','Skill, command, or prompt','Save','Cancel','Delete','Edit','Close','Insert a saved skill, command, or prompt. Press Enter separately to run.','Inserted into the active terminal or chat input without pressing Enter.','No quick actions yet. Save skills like /krill-orchestration, shell commands, or prompts to insert without pressing Enter.','Saved on this phone. Could not sync quick actions to desktop. Reconnect and save again.'])assert.notEqual(I.t(key),key,key+' Korean translation');
 // List snapshots refresh saved content, while an owner change fences a retained manual form.
 f.state.snapshot.quick_commands=[{name:'From desktop',text:'/desktop'}];f.view.update();assert(manager.textContent.includes('From desktop'));await find('Edit').action();const staleSave=find('Save'),staleForm=manager.querySelector('form'),draft=manager.querySelector('textarea');draft.value='unsaved';const calls=f.calls.length;f.ctx.transport={};f.view.update();assert.equal(manager.querySelector('form'),staleForm);assert.equal(draft.value,'unsaved');assert(staleSave.disabled);await staleSave.action();emit(staleForm,'submit');await f.flush();assert.equal(f.calls.length,calls);
 f.abort.abort();console.log('PASS terminal settings quick commands: stable list/manual IME draft on snapshots, owner fencing, add/edit/delete and server normalization/local mirror');
}
async function installedQuickCommandSettings(){
 const sessions=new Map([[7,{id:7,agent:'claude',state:'Working',groupKey:'repo',cwd:'/repo',title:'Current CLI',account_id:'own-account'}],[8,{id:8,agent:'pi',state:'Working',groupKey:'repo',cwd:'/repo/other'}],[9,{id:9,state:'Exited'}]]),groups=new Map([['repo',{label:'Project',sessions:[...sessions.values()]}]]),pending=new Map(),requests=[];
 const f=fixture('settings',{route:{subpage:'terminal'},state:{sessions,groups,snapshot:{capabilities:{command_catalog:true},quick_commands:[]}},rpc:req=>{requests.push(req);if(req.action==='save_quick_commands')return {quick_commands:req.commands};const d=deferred();pending.set(req.session,d);return d.promise;}});
 await f.byText('Find installed skills').action();assert.equal(f.calls.length,0,'settings never guesses another session');assert(f.root.textContent.includes('Choose a session to see its installed skills and commands.'));const select=f.all().find(n=>n.getAttribute('aria-label')==='Session for installed skills');assert.deepEqual(select.children.map(n=>n.value),['','7','8']);
 select.value='7';emit(select,'change');await f.flush();assert.equal(f.calls.at(-1).session,7);const search=f.find('.quick-catalog-search'),picker=f.find('.quick-catalog');search.value='review';search.focus();search.selectionStart=1;search.selectionEnd=4;emit(search,'compositionstart');emit(search,'input');for(let i=0;i<30;i++)f.view.update();assert.equal(f.find('.quick-catalog'),picker);assert.equal(f.find('.quick-catalog-search'),search);assert.equal(f.doc.activeElement,search);assert.equal(search.value,'review');assert.equal(search.selectionStart,1);assert.equal(search.selectionEnd,4);assert.equal(f.calls.length,1,'snapshots retain the in-flight catalog request');emit(search,'compositionend');
 select.value='8';emit(select,'change');await f.flush();pending.get(7).resolve({supported:true,items:[{name:'Wrong session',text:'/wrong'}]});pending.get(8).resolve({supported:true,items:[{name:'Pi review',text:'/skill:review',description:'review this project'}]});await f.flush();assert(!f.root.textContent.includes('Wrong session'));assert(f.root.textContent.includes('Pi review'));
 const matched=f.find('.quick-action-card'),matchedAdd=f.byText('Add to quick actions');search.focus();emit(search,'compositionstart');for(let i=0;i<30;i++)f.view.update();assert.equal(f.find('.quick-catalog'),picker);assert.equal(f.find('.quick-catalog-search'),search);assert.equal(f.find('.quick-action-card'),matched);assert.equal(f.doc.activeElement,search);assert.equal(search.value,'review');assert.equal(search.selectionStart,1);assert.equal(search.selectionEnd,4);assert.equal(f.calls.filter(r=>r.action==='command_catalog').length,2);emit(search,'compositionend');
 await matchedAdd.action();assert.equal(f.state.snapshot.quick_commands[0].text,'/skill:review');assert(f.byText('Already saved').disabled);const oldAdded=f.byText('Already saved');select.value='7';emit(select,'change');await f.flush();await oldAdded.action();assert.equal(f.calls.filter(r=>r.action==='command_catalog').length,2,'session result cached per picker open');search.value='';emit(search,'input');assert(f.root.textContent.includes('Wrong session'));
 // Usage, titles and state updates do not invalidate the selected session catalog.
 const resultRow=f.find('.quick-action-card');search.focus();search.selectionStart=0;search.selectionEnd=0;for(let i=0;i<30;i++)f.view.update();assert.equal(f.find('.quick-action-card'),resultRow);assert.equal(f.find('.quick-catalog-search'),search);assert.equal(f.doc.activeElement,search);assert.equal(search.selectionStart,0);assert.equal(search.selectionEnd,0);
 f.state.snapshot.accounts=[{usage:99}];sessions.get(7).title='Updated title';sessions.get(7).state='Done';f.view.update();assert.equal(f.find('.quick-action-card'),resultRow);assert(select.children.find(n=>n.value==='7').textContent.includes('Updated title'));assert(f.root.textContent.includes('Wrong session'));assert.equal(f.calls.filter(r=>r.action==='command_catalog').length,2);
 // Session choices follow live snapshots without replacing search/selection or refetching.
 sessions.set(10,{id:10,agent:'codex',state:'Working',groupKey:'repo'});sessions.get(8).exited=true;f.view.update();assert.deepEqual(select.children.map(n=>n.value),['','7','10']);assert.equal(select.value,'7');assert.equal(f.find('.quick-catalog-search'),search);assert.equal(f.doc.activeElement,search);assert.equal(f.calls.filter(r=>r.action==='command_catalog').length,2);sessions.get(8).exited=false;
 // A profile/account change does invalidate the visible list and retained actions.
 const stale=f.byText('Add to quick actions'),count=f.calls.length;sessions.get(7).account_id='replacement-account';f.view.update();assert(stale.disabled);assert(!f.root.textContent.includes('Wrong session'));assert(f.root.textContent.includes('Session context changed. Reopen installed skills.'));assert.equal(f.find('.quick-catalog-search'),search);await stale.action();assert.equal(f.calls.length,count);sessions.get(7).account_id='own-account';f.view.update();await stale.action();assert.equal(f.calls.length,count,'returning to an old context cannot revive retained actions');
 select.value='10';emit(select,'change');await f.flush();pending.get(10).resolve({supported:true,items:[{name:'New session',text:'/new'}]});await f.flush();const dead=f.byText('Add to quick actions'),beforeDead=f.calls.length;sessions.delete(10);f.view.update();assert(dead.disabled);assert.equal(select.value,'');assert.deepEqual(select.children.map(n=>n.value),['','7','8']);await dead.action();assert.equal(f.calls.length,beforeDead);assert(!f.root.textContent.includes('New session'));
 await f.byText('Back').action();assert.equal(f.doc.activeElement,f.byText('Find installed skills'));await f.byText('Find installed skills').action();const second=f.all().find(n=>n.getAttribute('aria-label')==='Session for installed skills');second.value='8';emit(second,'change');await f.flush();f.abort.abort();pending.get(8).resolve({supported:true,items:[{name:'After abort',text:'/late'}]});await f.flush();assert(!f.root.textContent.includes('After abort'));
 const noSession=fixture('settings',{route:{subpage:'terminal'},state:{snapshot:{capabilities:{command_catalog:true}}}});await noSession.byText('Find installed skills').action();assert(noSession.root.textContent.includes('No live sessions. Open a session to find installed skills.'));assert.equal(noSession.calls.length,0);noSession.abort.abort();
 const old=fixture('settings',{route:{subpage:'terminal'}});assert(!old.byText('Find installed skills'),'catalog requires daemon capability');assert(old.byText('+ Add'),'manual creation remains available');old.abort.abort();
 console.log('PASS installed quick actions settings: explicit live-session selection, per-open cache, stale session/account/abort guards, unrelated updates, no-session and old-daemon fallback');
}
async function sessionDetailsAudit(){
 const messages={outside_scope:'Diff is outside the current workspace scope.',no_current_diff:'No current diff for this file.'};
 let response={patch:'+current'},failure;const f=fixture('git',{route:{sessionId:7},rpc:req=>{if(req.path===null)return {files:['file.rs']};if(failure)throw failure;return response;},t:key=>'translated:'+key});await f.flush();
 const row=f.all().find(n=>n.className==='file-row');
 for(const [diff_status,message] of Object.entries(messages)){response={diff_status,patch:'+stale'};await row.action();assert(f.find('.git-patch').textContent.includes('translated:'+message));assert(!f.find('.git-patch').textContent.includes('+stale'));}
 failure=Error('diff exceeds limit');await row.action();const tooLarge='translated:Diff is too large to display on mobile (128 KB limit). Open this file on desktop to view the full diff.';assert.equal(f.find('.git-patch').textContent,tooLarge);assert.equal(f.find('.git-status').textContent,tooLarge);assert.equal(f.all().find(n=>n.className==='secondary').hidden,true);
 failure=Error('temporary failure');await row.action();assert.equal(f.all().find(n=>n.className==='secondary').hidden,false,'transient diff error still offers retry');f.abort.abort();
 const g=fixture('settings',{route:{subpage:'terminal'},state:{preferences:{orderedKeyIds:['tab','esc'],visibleKeyIds:['tab']}}});await g.byText('Reset order').action();assert.deepEqual(Array.from(g.state.preferences.orderedKeyIds),['esc','tab']);assert.deepEqual(Array.from(g.state.preferences.visibleKeyIds),['esc','tab']);assert(g.all().filter(n=>n.tagName==='INPUT'&&n.type==='checkbox').every(n=>n.checked));assert.deepEqual(g.all().filter(n=>n.className==='key-name mono'||n.className==='mono key-name').map(n=>n.textContent),['Esc','Tab']);for(const label of g.all().filter(n=>n.tagName==='LABEL'))assert(!label.querySelector('button'));g.abort.abort();
 console.log('PASS Git scope/no-current/oversize messages and transient Retry, terminal key reset defaults');
}
async function settings(){const f=fixture('settings'),link=label=>f.all().find(n=>n.tagName==='BUTTON'&&String(n.className).includes('setting-link')&&n.textContent===label);
 const choose=async(v,label,name,key,value)=>{const group=v.all().find(n=>n.getAttribute('role')==='group'&&n.getAttribute('aria-label')===label),choice=group.children.find(n=>n.getAttribute('aria-label')===name);await choice.action();assert.equal(v.state.preferences[key],value);assert.deepEqual(group.children.map(n=>n.getAttribute('aria-pressed')),group.children.map(n=>String(n===choice)));};
 await link('Terminal').action();assert.equal(f.routes.at(-1),'/settings/terminal');for(const removed of ['Appearance','Chat UI','Connection Log','Privacy','About Krill'])assert.equal(link(removed),undefined,`${removed} row removed`);assert.equal(f.byText('Install Krill'),undefined,'Install Krill row removed');assert(!f.find('.settings-nav'));assert.deepEqual(f.all().find(n=>n.getAttribute('role')==='group'&&n.getAttribute('aria-label')==='Theme').children.map(n=>n.getAttribute('aria-pressed')),['false','false','true'],'Dark is the default theme');for(const v of [['Default session view','Chat','defaultSessionView','chat'],['Language','한국어','language','ko'],['Theme','Light','appearance','light'],['UI text size','150%','uiTextScale',1.5]])await choose(f,...v);f.abort.abort();
 const invalid=fixture('settings',{state:{preferences:{defaultSessionView:'invalid'}}});assert.deepEqual(invalid.all().find(n=>n.getAttribute('role')==='group'&&n.getAttribute('aria-label')==='Default session view').children.map(n=>n.getAttribute('aria-pressed')),['false','true']);invalid.abort.abort();
 const legacy=fixture('settings',{route:{subpage:'appearance'}});assert(legacy.all().some(n=>n.getAttribute('role')==='group'&&n.getAttribute('aria-label')==='Theme'),'unknown settings subpage falls back to root Settings');assert(legacy.all().some(n=>n.tagName==='BUTTON'&&String(n.className).includes('setting-link')&&n.textContent==='Terminal'));legacy.abort.abort();
 const g=fixture('settings',{route:{subpage:'terminal'}});g.headers[0].onBack();assert.equal(g.routes.at(-1),'/settings');for(const check of g.all().filter(n=>n.tagName==='INPUT'&&n.type==='checkbox')){const label=g.all().find(n=>n.tagName==='LABEL'&&n.htmlFor===check.id);assert(label);assert(label.textContent.length);assert(!label.querySelector('button'));check.checked=false;emit(check,'change');assert(!g.state.preferences.visibleKeyIds.includes(check.id.replace('accessory-key-','')));}await g.all().find(n=>n.getAttribute('aria-label')==='Buffered input').action();assert.equal(g.state.preferences.inputMode,'buffered');g.abort.abort();
 // Verify theme-color meta + manifest.json #0a0a0a/#ffffff parity and system prefers-color-scheme change listener.
 const manifest=JSON.parse(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/manifest.json'),'utf8'));
 assert.equal(manifest.theme_color,'#0a0a0a');assert.equal(manifest.background_color,'#0a0a0a');
 const appLines=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8').split('\n'),media=Object.assign(new EventTarget(),{matches:false}),meta={content:'',setAttribute(_k,v){this.content=v;}},themeBox={state:{preferences:{}},M:{i18n:{configure(){}}},document:{documentElement:{dataset:{},style:{setProperty(){}}},querySelector:s=>s==='meta[name="theme-color"]'?meta:null},matchMedia:q=>q==='(prefers-color-scheme: light)'?media:null};
 themeBox.globalThis=themeBox;vm.createContext(themeBox);
 vm.runInContext([appLines.find(l=>l.trim().startsWith('function applyPreferences(')),appLines.find(l=>l.trim().startsWith('const colorSchemeMedia='))].join('\n')+'\nglobalThis.apply=applyPreferences;',themeBox);
 themeBox.apply();assert.equal(meta.content,'#0a0a0a','dark theme uses --surface-base #0a0a0a');
 themeBox.state.preferences.appearance='light';themeBox.apply();assert.equal(meta.content,'#ffffff','light theme uses --surface-base #ffffff');
 themeBox.state.preferences.appearance='system';media.matches=false;themeBox.apply();assert.equal(meta.content,'#0a0a0a');
 media.matches=true;media.dispatchEvent(new Event('change'));assert.equal(meta.content,'#ffffff','OS switch to light updates theme-color in system mode');
 media.matches=false;media.dispatchEvent(new Event('change'));assert.equal(meta.content,'#0a0a0a','OS switch to dark updates theme-color in system mode');
 themeBox.state.preferences.appearance='dark';media.matches=true;media.dispatchEvent(new Event('change'));assert.equal(meta.content,'#0a0a0a','explicit dark ignores OS light change');
 console.log('PASS SC-A11, CO-A14 and SC-Q01/Q02 support: checkbox label association excludes move buttons, installation reentry, persisted language/theme/UI scale and input mode segmented choices, settings page bar');}
async function renderPerformance(){
 const f=fixture('workspaces');f.state.snapshot.repositories=[{key:'repo',label:'Large project'}];f.state.snapshot.workspaces=[];f.state.preferences.workspaceTree={'repo:repo':true};for(let i=0;i<1000;i++){const g={key:'g'+i,label:'Same',repoKey:'repo',path:'/repo/'+i+'/same',sessions:[]};f.state.groups.set(g.key,g);}f.view.update();assert.equal(f.all().filter(n=>n.dataset.group).length,1000);assert(f.find('.workspace-list').classList.contains('large-workspace-list'));assert(f.all().find(n=>n.dataset.group==='g500').textContent.includes('500/same'),'indexed suffix remains unambiguous');const focused=f.all().find(n=>n.dataset.group==='g500');let attributeWrites=0;const setAttribute=focused.setAttribute.bind(focused);focused.setAttribute=(...args)=>{attributeWrites++;setAttribute(...args);};f.view.update();assert.equal(attributeWrites,0,'unchanged row signature skips DOM attributes');focused.focus();const first=f.state.groups.get('g0');f.state.groups.delete(first.key);f.state.groups.set(first.key,first);f.view.update();assert.equal(f.doc.activeElement,focused);assert.equal(f.all().filter(n=>n.dataset.group).at(-1).dataset.group,'g0');const composing=f.find('input');composing.value='Same';composing.selectionStart=1;composing.selectionEnd=3;composing.focus();emit(composing,'compositionstart');emit(composing,'input');f.view.update();assert.equal(f.doc.activeElement,composing);assert.equal(composing.value,'Same');assert.equal(composing.selectionStart,1);assert.equal(composing.selectionEnd,3);emit(composing,'compositionend');f.abort.abort();
 const fields=fixture('workspaces'),{a,ga}=groups(fields);fields.state.preferences.workspaceTree['w:g-a']=true;fields.view.update();const stable=fields.all().find(n=>n.dataset.group==='g-a'),terminal=fields.all().find(n=>n.dataset.session==='1');terminal.focus();a.title='New terminal';a.agent='claude';a.lines=['new last line'];a.state='Blocked';a.branch='feature';ga.label='Renamed';ga.depth=2;ga.primary=true;fields.view.update();assert.equal(fields.all().find(n=>n.dataset.group==='g-a'),stable);assert.equal(fields.all().find(n=>n.dataset.session==='1'),terminal);assert.equal(fields.doc.activeElement,terminal);assert.equal(stable.parentElement.parentElement.dataset.depth,'2');assert(stable.textContent.includes('Renamed'));assert(stable.textContent.includes('feature'));assert(terminal.textContent.includes('New terminal'));assert(terminal.textContent.includes('new last line'));assert(terminal.getAttribute('aria-label').includes('Blocked'));assert(terminal.descendants().some(n=>n.getAttribute('class')==='agent-icon claude'));a.exited=true;fields.view.update();assert(!fields.all().some(n=>n.dataset.session==='1'),'exited session removal invalidates signature');fields.abort.abort();
 const g=fixture('git',{route:{sessionId:7},rpc:req=>req.path?{patch:req.path==='large'?'+line\n'.repeat(800):'small current'}:{files:['large','small']}});await g.flush();let latest=0;g.ctx.rpcLatest=(key,request)=>{assert.equal(key,'git-patch:7');latest++;return g.ctx.rpc(request);};g.byText('large').action();await g.flush();const patch=g.find('.git-patch');assert.equal(patch.children.length,256,'large patch yields after one bounded DOM chunk');assert.equal(patch.getAttribute('aria-busy'),'true');await g.byText('small').action();await g.flush();for(const [id,t] of [...g.timers]){g.timers.delete(id);t.fn();}await g.flush();assert.equal(patch.textContent,'small current\n');assert.equal(patch.children.length,1);assert.equal(patch.getAttribute('aria-busy'),'false');assert.equal(latest,2);const complete=g.byText('large').action();await g.flush();assert.equal(patch.children.length,256);for(let n=0;n<4;n++){for(const [id,t] of [...g.timers]){g.timers.delete(id);t.fn();}await g.flush();}await complete;assert.equal(patch.children.length,801,'all diff lines eventually render in order');assert.equal(patch.textContent,'+line\n'.repeat(800)+'\n');g.byText('large').action();await g.flush();const stopped=patch.children.length;g.abort.abort();for(const [id,t] of [...g.timers]){g.timers.delete(id);t.fn();}await g.flush();assert.equal(patch.children.length,stopped,'aborted render creates no later chunks');
 const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/home.js'),'utf8'),storage=new Map(),events=new EventTarget();let parses=0;const context=vm.createContext({JSON:{parse:value=>{parses++;return JSON.parse(value);},stringify:JSON.stringify},localStorage:{getItem:key=>storage.get(key),setItem:(key,value)=>storage.set(key,value),removeItem:key=>storage.delete(key)},addEventListener:events.addEventListener.bind(events)});vm.runInContext(source,context);const recent=context.KrillMobile.recentSession;recent.write('one',{id:7},{key:'g'});recent.read('one');const parseCount=parses;for(let n=0;n<100;n++)recent.list('one');assert.equal(parses,parseCount,'unchanged recent JSON parses once across repeated host reads');const event=new Event('storage');event.key='krill-mobile-last-session-v1';events.dispatchEvent(event);recent.read('one');assert.equal(parses,parseCount+1,'cross-tab storage event invalidates memo');
 console.log('PASS large ordered workspace suffix index/focus, chunked patch yield and stale-chunk cancellation');
}
async function mobileAuditIdentity(){
 const f=fixture('home'),M=f.M,ctx=f.ctx;
 for(const [session,expected] of [
  [{agent:'pi'},'pi'],[{agent:'/opt/Pi.EXE'},'pi'],[{agent:'π'},'pi'],
  [{agent:'/bin/ZSH',foreground:'/opt/Pi.EXE',title:'Renamed task'},'pi'],
  [{agent:'',title:'Working · π: task'},'pi'],[{agent:'zsh',title:'pi task'},'pi'],
  [{agent:'',title:'rapid shipping'},''],[{agent:'',title:'my-pi-bot'},''],
  [{agent:'custom-pi',foreground:'codex',title:'Claude task'},'custom-pi'],
  [{agent:'claude',foreground:'pi',title:'Renamed task'},'claude'],
  [{agent:'zsh',foreground:'node',title:'Codex task'},'codex']])assert.equal(M.sessionAgent(session),expected,JSON.stringify(session));
 const pi=M.agentIcon('pi',ctx);assert.equal(pi.getAttribute('class'),'agent-icon pi');assert.equal(pi.getAttribute('viewBox'),'0 0 800 800');assert.equal(pi.children.length,2);assert.equal(pi.children[0].getAttribute('fill-rule'),'evenodd');assert.equal(M.agentName('π'),'Pi');
 f.ctx.connections=[{id:'pc',name:'PC',connection:{phase:'ready'},snapshot:{capabilities:{bots:true},sessions:[{id:1,agent:'zsh',foreground:'pi',title:'Renamed task',worktree_key:'w'}]}}];f.view.update();let row=f.find('.recent-session-row');assert(row.textContent.includes('Pi'));assert(row.descendants().some(n=>n.getAttribute('class')==='agent-icon pi'));f.ctx.connections[0].snapshot.sessions[0].foreground='codex';f.view.update();assert.equal(f.find('.recent-session-row'),row);assert(row.textContent.includes('Codex'));assert(!row.descendants().some(n=>n.getAttribute('class')==='agent-icon pi'));f.abort.abort();
 const w=fixture('workspaces'),{a:workspaceSession}=groups(w);workspaceSession.agent='zsh';workspaceSession.foreground='pi';w.state.preferences.workspaceTree['w:g-a']=false;w.view.update();const summary=w.all().find(n=>n.dataset.group==='g-a');assert(summary.textContent.includes('Pi'));assert(summary.descendants().some(n=>n.getAttribute('class')==='agent-icon pi'));w.abort.abort();
 const a=fixture('accounts',{rpc:()=>({accounts:[{id:'a',provider:'Antigravity',label:'Default',active:true,host_default:true,usage:{}},{id:'o',provider:'OpenCode',label:'Default',active:true,host_default:true,usage:{}}]})});await a.flush();for(const id of ['agy','opencode']){const mark=a.all().find(n=>n.getAttribute('class')===`agent-icon ${id}`);assert(mark);assert.equal(mark.children.at(-1).getAttribute('class'),'layer','accounts reuse the session multicolor mark');}a.abort.abort();
 const g=fixture('orchestration',{state:{snapshot:{orchestration:{gates:[{id:'empty',status:'pending',question:'No options supplied'}]}}}});assert.equal(g.find('.gate-options').children.length,0,'missing options cannot invent Approve/Deny defaults');assert.equal(g.calls.length,0);g.state.snapshot.orchestration.gates[0].options=['Delete everything','Wait'];g.view.update();assert(g.find('.gate-options').children.every(n=>n.className==='secondary'),'opaque server choices have equal priority and no inferred danger/default');g.abort.abort();
 console.log('PASS M02/M03/M09: Pi path/foreground/token boundaries, custom identity, keyed home/workspace marks, multicolor accounts, no invented gate defaults');
}
async function mobileAuditScanner(){
 const f=fixture('home'),Element=f.root.constructor,doc=f.doc,trigger=new Element('button'),container=new Element('div');doc.body.append(trigger,container);trigger.focus();doc.createElement=tag=>new Element(tag);doc.getElementById=()=>container;
 Element.prototype.showModal=function(){this.open=true;this.modalCalls=(this.modalCalls||0)+1;};Element.prototype.close=function(){this.open=false;this.dispatchEvent(new Event('close'));};Element.prototype.click=function(){this.clicked=(this.clicked||0)+1;};
 const pending=deferred(),timers=new Map();let timerId=0,stops=0,closedSheets=0;const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8'),scan=source.slice(source.indexOf(' function scanQr(){'),source.indexOf(' function pasteLink(){'));
 const sandbox={document:doc,el:(...v)=>new Element(...v),t:s=>'KO:'+s,button(label,action,cls){const b=new Element('button',label,cls);b.action=action;return b;},closeSheet(){closedSheets++;},navigator:{mediaDevices:{getUserMedia:()=>pending.promise}},isSecureContext:true,setTimeout:fn=>{timers.set(++timerId,fn);return timerId;},clearTimeout:id=>timers.delete(id),act:fn=>fn(),endpoint:{adopt(){throw Error('invalid QR');}},photoQr:async()=>null,pasteLink(){},readQr:async()=>null,pairWith(){throw Error('unexpected pairing');}};
 vm.createContext(sandbox);vm.runInContext('let scanner;'+scan+'globalThis.run=scanQr;globalThis.current=()=>scanner;',sandbox);sandbox.run();let view=container.children.at(-1);assert.equal(view.tagName,'DIALOG');assert.equal(view.modalCalls,1);assert.equal(view.getAttribute('aria-label'),'KO:Scan QR code');assert.equal(doc.activeElement,view.querySelector('.scanner-close'));const cancelled=new Event('cancel',{cancelable:true});view.dispatchEvent(cancelled);assert(cancelled.defaultPrevented);assert.equal(view.isConnected,false);assert.equal(doc.activeElement,trigger);assert.equal(sandbox.current(),null);pending.resolve({getTracks:()=>[{stop(){stops++;}}]});await f.flush();assert.equal(stops,1,'camera resolving after Escape stops every track');assert.equal(timers.size,0);
 sandbox.navigator.mediaDevices={getUserMedia:async()=>({getTracks:()=>[{stop(){stops++;}}]})};sandbox.run();view=container.children.at(-1);view.querySelector('video').play=async()=>{};await f.flush();view.querySelector('.scanner-close').action();view.dispatchEvent(new Event('close'));assert.equal(stops,2,'close/cancel/close-event cleanup is idempotent');assert.equal(doc.activeElement,trigger);assert.equal(timers.size,0);
 const decoded=deferred();let adopted=0;sandbox.readQr=()=>decoded.promise;sandbox.endpoint.adopt=()=>{adopted++;return null;};sandbox.run();view=container.children.at(-1);const video=view.querySelector('video');video.videoWidth=video.videoHeight=320;video.play=async()=>{};await f.flush();sandbox.current()();decoded.resolve('late QR');await f.flush();assert.equal(adopted,0,'a decode completing after close must never start pairing');assert.equal(stops,3);sandbox.isSecureContext=false;sandbox.run();view=container.children.at(-1);assert(view.classList.contains('photo-only'));assert(view.querySelector('.scanner-hint').textContent.startsWith('KO:Take a photo'));assert(view.querySelector('video').hidden);assert.equal(view.querySelector('input').clicked,1);sandbox.current()();assert.equal(closedSheets,4);f.abort.abort();
 delete Element.prototype.showModal;delete Element.prototype.close;delete Element.prototype.click;
 console.log('PASS M01/M04/M05: scanner native modal entry/focus, Escape cleanup/restore, late stream release, idempotent close, localized photo-only fallback');
}

async function mobileAuditPairingAndShell(){
 const f=fixture('home'),E=f.root.constructor,root=new E('main');f.doc.body.append(root);let scans=0,pastes=0;const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8'),line=source.split('\n').find(l=>l.includes("if((!state.snapshot||state.cached")),guard=line.slice(0,line.indexOf("if(route.screen==='session'){"));
 const sandbox={root,state:{snapshot:null,connection:{message:'Not connected'}},route:{screen:'accounts'},endpoint:{profiles:()=>[]},header:()=>new E('header'),el:(...args)=>new E(...args),t:s=>'KO:'+s,button:(label,action,cls)=>{const node=new E('button',label,cls);node.action=action;return node;},scanQr:()=>scans++,pasteLink:()=>pastes++,go(){}};vm.createContext(sandbox);vm.runInContext('function draw(){'+guard+'}globalThis.draw=draw;',sandbox);sandbox.draw();assert(root.textContent.includes('KO:Not connected'));const buttons=root.querySelectorAll('button');assert.equal(buttons.length,2);assert.equal(buttons[0].className,'primary');buttons[0].action();buttons[1].action();assert.equal(scans,1);assert.equal(pastes,1);
 root.replaceChildren();sandbox.endpoint.profiles=()=>[{id:'saved'}];sandbox.draw();assert.equal(root.querySelectorAll('button').length,0,'a paired but disconnected desktop keeps its existing reconnect flow');
 const locale=vm.createContext({navigator:{language:'ko'}});vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/i18n.js'),'utf8'),locale);const I=locale.KrillMobile.i18n;for(const key of ['Session not found','Send prompt','Prompt','Write a prompt…','Terminal text','Terminal keys','Latest terminal output','Touch and hold to select and copy.','Pairing link','Copy the pairing link in desktop Settings → Mobile and paste it here.','Reading photo…','Take a photo of the QR code in desktop Settings → Mobile.','Camera is unavailable. Take a photo of the QR code instead.'])assert.notEqual(I.t(key),key,key);assert.equal(I.t('Terminal #{0} will be stopped.',7),'터미널 #7이 종료됩니다.');assert.equal(I.t('Enter a new message'),'새 메시지를 입력하세요');assert.equal(I.t('Start a conversation'),'새 대화를 시작하세요');assert.equal(I.t('Model unknown'),'Model unknown');assert.equal(I.t('Effort unknown'),'Effort unknown');
 const assetRoot=path.join(__dirname,'../src/mobile/assets'),home=fs.readFileSync(path.join(assetRoot,'screens/home.js'),'utf8'),sw=fs.readFileSync(path.join(assetRoot,'sw.js'),'utf8'),indexHtml=fs.readFileSync(path.join(assetRoot,'index.html'),'utf8'),zlib=require('node:zlib');assert(home.includes("logo.src='icon.svg'"));assert(sw.split('\n').find(l=>l.includes('SHELL=')).includes('icon.svg'),'home logo is in the direct-origin offline shell');const inventory=indexHtml.split('<meta name="krill-update" content="1 ')[1].split('"')[0].split(' ');assert(inventory.length>=23);for(const file of inventory)assert.deepEqual(zlib.gunzipSync(fs.readFileSync(path.join(assetRoot,file+'.gz'))),fs.readFileSync(path.join(assetRoot,file)),file+' gzip mirror');f.abort.abort();
 console.log('PASS M05/M10/M13: Korean copy/interpolation, unpaired Accounts scan/paste CTA, saved-host flow, offline logo inventory and gzip mirrors');
}

async function installHandoffFragments(){
 const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8'),iosFn=source.slice(source.indexOf(' async function installIos(){'),source.indexOf(' async function installAndroid(){')),androidFn=source.slice(source.indexOf(' async function installAndroid(){'),source.indexOf(' const install=M.install='));
 const run=async(code,fragment)=>{const replaced=[],sheets=[],sandbox={credentials:{device_id:'dev'},transport:{rpc:async req=>{assert.equal(req.action,'install_handoff');return {fragment};}},location:{pathname:'/m/0123456789abcdef/',search:''},history:{replaceState:(_s,_t,url)=>replaced.push(url)},install:{prompt:null,show(){}},sheet:title=>sheets.push(title),t:s=>s,el:()=>({append(){}})};vm.createContext(sandbox);vm.runInContext(code+'\nglobalThis.entry=typeof installIos==="function"?installIos:installAndroid;',sandbox);await sandbox.entry();return {replaced,sheets};};
 const pair='a'.repeat(64),key='B_-0123456789abcdefghijklmnopqrstuvwxyzABCD',r='c'.repeat(32),direct=`pair=${pair}&key=${key}`;
 for(const [platform,code] of [['ios',iosFn],['android',androidFn]]){
  for(const valid of [direct,`${direct}&r=${r}`,`${direct}&r=${'d'.repeat(32)}`]){
   const res=await run(code,valid);assert.deepEqual(res.replaced,[`/m/0123456789abcdef/#${valid}&install=1`],`${platform}: ${valid}`);assert.equal(res.sheets.length,1);
  }
  for(const invalid of [null,123,'',`${direct}&t=krill.example.com&r=${r}`,`pair=${'A'.repeat(64)}&key=${key}`,`pair=${'a'.repeat(63)}&key=${key}`,`pair=${pair}&key=${key.slice(1)}`,`${direct}&r=`,`${direct}&r=${'C'.repeat(32)}`,`${direct}&r=${'c'.repeat(31)}`,`${direct}&r=${'c'.repeat(33)}`,`${direct}&t=https://krill.example.com&r=${r}`,`${direct}&t=krill.example.com/path&r=${r}`,`${direct}&t=localhost&r=${r}`,`${direct}&t=-bad.example.com&r=${r}`,`${direct}&t=bad-.example.com&r=${r}`,`${direct}&t=a..b.com&r=${r}`,`${direct}&t=${'a'.repeat(64)}.example.com&r=${r}`,`${direct}&t=user@krill.example.com&r=${r}`,`${direct}&r=${r}&t=krill.example.com`,`${direct}&r=${r}&r=${r}`,`${direct}&install=1`,`${direct}&extra=1`]){
   await assert.rejects(()=>run(code,invalid),/Could not prepare the Home Screen app/,`${platform} must reject ${invalid}`);
  }
 }
 // End-to-end install handoff lifecycle: live snapshot() updates continue while open and after close, hash restores to #/host/local, and forward + back() never calls location.reload().
 {
  const f=fixture('workspaces'),E=f.root.constructor,lines=source.split('\n'),pick=m=>lines.find(l=>l.trim().startsWith(m));
  const dialog=new E('dialog'),sheetBody=new E('div'),rootNode=new E('main'),bannerNode=new E('div');
  dialog.open=false;dialog.showModal=()=>{dialog.open=true;};dialog.close=()=>{dialog.open=false;dialog.dispatchEvent(new Event('close'));};dialog.removeAttribute=k=>{delete dialog.attrs[k];};
  sheetBody.querySelector=()=>null;f.doc.body.classList.remove=v=>{f.doc.body.classList.toggle(v,false);};
  dialog.append(sheetBody);f.doc.body.append(rootNode,dialog);
  const win=new EventTarget(),entries=['#/host/local'];let idx=0,reloads=0,screenUpdates=0,renderedRoutes=[];
  const loc={pathname:'/m/0123456789abcdef/',search:'',reload(){reloads++;}};
  Object.defineProperty(loc,'hash',{get:()=>entries[idx],set:v=>{if(v===entries[idx])return;entries.splice(idx+1,Infinity,v);idx++;win.dispatchEvent(new Event('hashchange'));}});
  const snapIdx=lines.findIndex(l=>l.trim().startsWith('function snapshot(')),hashLine=lines.find(l=>l.includes("globalThis.addEventListener('hashchange'")),hashHandler=hashLine.slice(hashLine.indexOf("globalThis.addEventListener('hashchange'"));
  f.doc.addEventListener=()=>{};
  const box={
   document:f.doc,location:loc,history:{replaceState:(_a,_b,url)=>{entries[idx]=url.slice(url.indexOf('#'));},back:()=>{idx--;win.dispatchEvent(new Event('hashchange'));}},
   addEventListener:win.addEventListener.bind(win),URLSearchParams,AbortController,Date,
   credentials:{device_id:'dev'},endpoint:{profiles:()=>[{id:'local',active:true}],describeHost(){}},
   transport:{ready:true,rpc:async req=>{assert.equal(req.action,'install_handoff');return {fragment:direct};}},
   state:{snapshot:{workspaces:[{key:'w1',label:'Initial'}],sessions:[]},connection:{phase:'ready',message:'Connected'},groups:new Map(),sessions:new Map(),expected:new Map(),preferences:{}},
   root:rootNode,banner:bannerNode,dialog,sheetBody,lastFocus:null,lastTrigger:null,routeKey:'#/host/local',activeRouteScreen:'workspaces',
   screen:null,abort:null,hostGeneration:1,deepLink:null,parameters:new URLSearchParams(),
   ios:true,android:false,installed:()=>false,landing:false,scanner:null,tabletMedia:null,tabletList:null,
   el:(...v)=>new E(...v),icon:()=>new E('svg'),t:s=>s,error:e=>{throw e;},savePreferences(){},
   button:(label,action,cls='')=>{const b=new E('button',label,cls);b.setAttribute('aria-label',label);b.action=()=>Promise.resolve().then(action);return b;},
   sessionScope:()=>'scope',offerInstall:()=>false,M:{screens:{},recentSession:{resolve:()=>null}}
  };
  box.globalThis=box;
  box.render=()=>{const r=box.parseRoute();const currentHash=typeof box.getInstallRestore()==='string'&&loc.hash.includes('&install=1')?box.getInstallRestore():loc.hash;box.routeKey=currentHash;box.activeRouteScreen=r.screen;renderedRoutes.push(r.screen);rootNode.replaceChildren(new E('section','','workspaces screen'));box.screen={update(){screenUpdates++;}};};
  vm.createContext(box);
  vm.runInContext([
   pick('let routeStack='),pick('const hostRoute='),pick('const sessionPath='),pick('const go='),pick('function back('),
   pick('function restoreFocus()'),pick('function closeSheet()'),pick("dialog.addEventListener('close'"),pick('function sheet('),
   pick('const pairingLink='),iosFn,androidFn,pick('const install=M.install='),pick('function parseRoute('),
   pick('let imeComposing='),pick('function update(){'),pick('function group(data)'),
   'let pendingRecent=null;',lines[snapIdx],lines[snapIdx+1],lines[snapIdx+2],lines[snapIdx+3],hashHandler
  ].join('\n')+'\nglobalThis.parseRoute=parseRoute;globalThis.getInstallRestore=()=>installRestoreHash;globalThis.runIos=installIos;globalThis.runAndroid=installAndroid;globalThis.closeSheet=closeSheet;globalThis.snapshot=snapshot;globalThis.go=go;globalThis.back=back;globalThis.install=install;',box);
  box.render();
  // 1) iOS install sheet open -> live snapshot() still updates -> closeSheet restores #/host/local -> forward + back() does not reload.
  await box.runIos();
  assert.equal(dialog.open,true);
  assert.equal(loc.hash,`#${direct}&install=1`);
  const beforeOpenUpdate=screenUpdates;
  box.snapshot({workspaces:[{key:'w1',label:'Updated while sheet open'}],sessions:[{id:1,worktree_key:'w1',state:'Working'}]});
  assert.equal(screenUpdates,beforeOpenUpdate+1,'live snapshot updates continue while iOS install sheet is open');
  assert.equal(box.state.groups.get('w1').label,'Updated while sheet open');
  box.closeSheet();
  assert.equal(dialog.open,false);
  assert.equal(loc.hash,'#/host/local','closing install sheet restores #/host/local');
  box.snapshot({workspaces:[{key:'w1',label:'Updated after close'}],sessions:[{id:1,worktree_key:'w1',state:'Waiting'}]});
  assert.equal(screenUpdates,beforeOpenUpdate+2,'live snapshot updates continue after closing install sheet');
  assert.equal(box.state.groups.get('w1').label,'Updated after close');
  box.go('/settings');
  assert.equal(loc.hash,'#/settings');
  box.back();
  assert.equal(loc.hash,'#/host/local','Back returns to #/host/local');
  assert.equal(reloads,0,'Back never triggers location.reload()');
  // 2) Android install sheet with deferred beforeinstallprompt -> keeps &install=1 until install.show() settles, then restores #/host/local.
  const choice=deferred();let prompted=0;
  box.install.prompt={prompt:async()=>{prompted++;},userChoice:choice.promise};
  await box.runAndroid();
  assert.equal(dialog.open,true);
  assert.equal(loc.hash,`#${direct}&install=1`);
  const installBtn=sheetBody.descendants().find(n=>n.tagName==='BUTTON'&&n.getAttribute('aria-label')==='Install');
  const pendingInstall=installBtn.action();
  await f.flush();
  assert.equal(dialog.open,false);
  assert.equal(prompted,1);
  assert.equal(loc.hash,`#${direct}&install=1`,'Android Install button keeps handoff hash while native prompt is open');
  box.snapshot({workspaces:[{key:'w1',label:'Updated during Android prompt'}],sessions:[{id:1,worktree_key:'w1',state:'Done'}]});
  assert.equal(screenUpdates,beforeOpenUpdate+3,'live snapshot updates continue while Android native prompt awaits userChoice');
  choice.resolve({outcome:'dismissed'});
  await pendingInstall;
  assert.equal(loc.hash,'#/host/local','settling install.show() restores #/host/local');
  box.go('/settings');box.back();
  assert.equal(loc.hash,'#/host/local');assert.equal(reloads,0);
  f.abort.abort();
 }
 console.log('PASS REMOTE-001: iOS/Android install_handoff accepts direct, relay and fixed/named tunnel fragments and rejects malformed fragments');
}

function exitedSessionProjection(){
 const f=fixture('workspaces'),source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8'),sandbox={state:f.state,M:f.M};vm.createContext(sandbox);vm.runInContext(source.split('\n').find(l=>l.includes('function group(data)'))+'globalThis.groupSnapshot=group;',sandbox);
 const sessions=[{id:1,worktree_key:'a',cwd:'/repo/a',state:'Working'},{id:2,worktree_key:'a',cwd:'/repo/a',state:'Done',exited:true},{id:3,worktree_key:'a',cwd:'/repo/a',state:'Exited'},{id:4,cwd:'/old',exited:true},{id:5,worktree_key:'gone',state:'Exited'},{id:6,worktree_key:'empty',exited:true}];
 f.state.expected=new Map([[2,{at:Date.now(),session:{id:2,groupKey:'a'}}]]);
 const data={workspaces:[{key:'a',path:'/repo/a'},{key:'empty',path:'/repo/empty'}],sessions};sandbox.groupSnapshot(data);
 assert.deepEqual([...f.state.sessions.keys()],[1],'both exit flags exclude path-backed sessions');assert.equal(f.state.expected.size,0,'an exited snapshot removes pending create placeholders');assert.deepEqual([...f.state.groups.keys()],['a','empty']);assert.equal(f.state.groups.get('empty').sessions.length,0,'catalog workspaces survive their last terminal');
 const projected=f.M.workspaceGroups(data);assert.deepEqual(Array.from(projected,g=>g.key),['a','empty']);assert.deepEqual(Array.from(projected[0].sessions,s=>s.id),[1]);assert.equal(projected[1].sessions.length,0);f.state.expected.set(9,{at:Date.now(),session:{id:9,groupKey:'a',exited:true}});sandbox.groupSnapshot(data);assert(!f.state.sessions.has(9),'a locally ended pending terminal cannot be restored by a delayed snapshot');assert.equal(f.state.expected.size,0);f.abort.abort();
 console.log('PASS exited snapshot/legacy workspace projection removes closed terminals and pending placeholders while retaining catalog workspaces');
}


async function layoutAnchors(){
 // Supply geometry explicitly: the shared fixture otherwise has clientHeight=0 semantics.
 const geometry=(f,port,predicate)=>{port.clientHeight=100;port.getBoundingClientRect=()=>({top:0,bottom:100});port.querySelectorAll=()=>f.all().filter(n=>port.contains(n)&&predicate(n));};
 const h=fixture('home');const host={id:'one',name:'PC',connection:{phase:'ready'},snapshot:{capabilities:{bots:true},sessions:[{id:1,state:'Working',title:'Build',worktree_key:'w1'}],orchestration:{}}};h.ctx.connections=[host];h.view.update();const home=h.find('.bots-list'),card=h.find('.recent-session-row');home.scrollTop=88;geometry(h,home,n=>n.classList.contains('recent-session-row'));card.getBoundingClientRect=()=>({top:200+(h.find('.todo-summary-banner')?120:0)-home.scrollTop,bottom:300+(h.find('.todo-summary-banner')?120:0)-home.scrollTop});card.focus();const y=card.getBoundingClientRect().top;host.snapshot.orchestration.gates=[{id:'gate',status:'pending'}];h.view.update();assert.equal(card.getBoundingClientRect().top,y);assert.equal(home.scrollTop,208);host.snapshot.orchestration.gates=[];h.view.update();assert.equal(home.scrollTop,88);assert(!h.find('.todo-summary-banner'));h.abort.abort();
 const w=fixture('workspaces');const {ga}=groups(w);w.view.update();const port=w.find('.workspace-list'),row=w.all().find(n=>n.dataset.group===ga.key),node=row.parentElement.parentElement;assert.equal(node.style.containIntrinsicBlockSize,'auto 78px','phone workspace terminals start folded (Orca lists workspaces only)');port.scrollTop=88;geometry(w,port,n=>n.dataset.group);const rowTop=()=>{const rows=port.querySelectorAll();return rows.indexOf(row)*100-port.scrollTop;};for(const r of port.querySelectorAll())r.getBoundingClientRect=()=>({top:port.querySelectorAll().indexOf(r)*100-port.scrollTop,bottom:port.querySelectorAll().indexOf(r)*100+100-port.scrollTop});row.focus();const before=rowTop();const first={key:'new',label:'New first',repoKey:'repo',pinned:true,sessions:[]};w.state.groups=new Map([[first.key,first],...w.state.groups]);w.view.update();assert.equal(rowTop(),before);w.abort.abort();
 const o=fixture('orchestration',{state:{snapshot:{orchestration:{questions:[{id:'q',to:'user',summary:'Question'}]}}}}),op=o.find('#orchestration'),question=o.find('.gate-card'),input=o.find('textarea');geometry(o,op,n=>n.classList.contains('gate-card'));op.scrollTop=177;question.getBoundingClientRect=()=>({top:op.children.indexOf(question)*300-op.scrollTop,bottom:op.children.indexOf(question)*300+300-op.scrollTop});input.focus();assert(o.find('.action-status').hidden);const qY=question.getBoundingClientRect().top;o.state.snapshot.orchestration.gates=[{id:'g',question:'New gate',status:'pending',options:['Proceed']}];o.view.update();assert.equal(question.getBoundingClientRect().top,qY);assert.equal(op.scrollTop,477);assert.equal(o.find('textarea'),input);o.abort.abort();
 const settings=fixture('settings',{route:{subpage:'terminal'}}),settingsPort=settings.find('.settings-list');settingsPort.scrollTop=900;const keyGroup=settings.all().find(n=>n.dataset.key==='esc').parentElement.parentElement;keyGroup.querySelector=selector=>keyGroup.descendants().find(n=>n.dataset.key==='esc'&&(!selector.includes('data-step')||n.dataset.step==='1')&&!n.disabled);const oldFocus=settings.root.constructor.prototype.focus;settings.root.constructor.prototype.focus=function(options){this.focusOptions=options;settings.doc.activeElement=this;};settings.all().find(n=>n.dataset.key==='esc'&&n.dataset.step==='1').action();assert.equal(settingsPort.scrollTop,900);assert.equal(settings.doc.activeElement.focusOptions.preventScroll,true);settings.root.constructor.prototype.focus=oldFocus;settings.abort.abort();
 // Reserve height before the first chunk, clear stale text immediately and restore a same-file reader.
 const g=fixture('git',{route:{sessionId:7},rpc:req=>req.path?{patch:'+line\n'.repeat(800)}:{files:['large']}});await g.flush();let work=g.byText('large').action();await g.flush();const patch=g.find('.git-patch');assert(parseFloat(patch.style.minHeight)>=16369);for(let i=0;i<4;i++){for(const [id,timer] of [...g.timers]){g.timers.delete(id);timer.fn();}await g.flush();}await work;assert.equal(patch.style.minHeight,'');patch.offsetHeight=16400;patch.scrollTop=45;g.find('.detail-list').scrollTop=10000;let pending=deferred();g.ctx.rpc=async req=>req.path?{patch:'+line\n'.repeat(800)}:pending.promise;work=g.byText('Refresh').action();assert.equal(patch.style.minHeight,'16400px');assert(!patch.textContent.includes('+line'));assert.equal(g.find('.detail-list').scrollTop,10000);pending.resolve({files:['large']});await g.flush();for(let i=0;i<4;i++){for(const [id,timer] of [...g.timers]){g.timers.delete(id);timer.fn();}await g.flush();}await work;assert.equal(g.find('.detail-list').scrollTop,10000);assert.equal(patch.scrollTop,45);assert.equal(patch.style.minHeight,'');
 g.state.preferences.uiTextScale=1.5;work=g.byText('large').action();await g.flush();assert(parseFloat(patch.style.minHeight)>=24538,'150% UI scale scales Git patch height reservation');g.abort.abort();
 const wm=fixture('workspaces',{state:{snapshot:{host:{display_name:'PC'},repositories:[{key:'r1',label:'ColorRepo',badge_color:'#ff0000'},{key:'r2',label:'EmojiRepo',icon:{kind:'emoji',value:'🦀'}}]}}});wm.byText('Workspace Options').action();wm.find('.project-picker').action();const dots=wm.all().filter(n=>n.classList.contains('project-choice-dot'));assert.equal(dots[0].style.backgroundClip,'padding-box');assert.equal(dots[1].style.backgroundClip,'');wm.abort.abort();
 const css=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.css'),'utf8');assert(css.includes('#mobile-app .copy-log,.copy-log{min-width:calc(80px * var(--ui-scale,1))'));
 console.log('PASS CLS screen anchors: Home inserts/removals, workspace tree estimates/reorder, focused requests, key moves and Git refresh/chunks');
}
async function tabletRoutePreservation(){
 const f=fixture('home'),E=f.root.constructor,source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8'),lines=source.split('\n'),sidebar=lines.filter(l=>l.startsWith(' let pendingRecent=')||l.startsWith(' function syncTablet()')).join('\n'),render=source.slice(source.indexOf(' function render(){'),source.indexOf(' // IME'));
 let mounted=0,cleaned=0,updates=0,sidebarCtx,sessionCtx;const rpc=[],transport={ready:true,rpc:request=>{rpc.push(request);return Promise.resolve({});}},sandbox={root:f.root,document:f.doc,location:{hash:'#/s/7'},activeRouteScreen:'home',routeKey:'',state:f.state,hostGeneration:1,transport,screen:null,abort:null,banner:new E('div'),endpoint:{profiles:()=>[{id:'pc',active:true}]},routeAssets:{},M:{screens:{workspaces(root,ctx){mounted++;sidebarCtx=ctx;root.append(new E('div','','workspace-list'));return {cleanup(){cleaned++;},update(){updates++;}};},session(root,ctx){sessionCtx=ctx;root.append(ctx.header('Session','/'));return {};}}},AbortController,matchMedia:()=>({matches:true}),parseRoute:()=>({screen:'session'}),el:(...v)=>new E(...v),header:()=>{const h=new E('header');h.append(new E('button'),new E('h1','Session'));return h;}};
 for(const key of ['button','icon','go','setRoute','back','backTo','t','install','dot','rollup','groupRoute','linkRow','sheet','closeSheet','longPress','savePreferences','rememberSession','resumeLastSession','scanQr','pasteLink','newWorkspace','sessionKey','selectHost','forgetHost','renameHost','hostRoute','getConnections','filteredConnections','openRecent','reconnectHost','photo','toast','error','act','createSession','setDeviceFilter','deviceFilterPill','syncDevicePills','syncTabBar','homeMode','rpcForHost','refreshHost','activateHost'])sandbox[key]=f.ctx[key]||(()=>{});sandbox.filteredConnections=()=>f.ctx.connections||[];sandbox.homeMode=()=>'bots';sandbox.deviceFilterPill=()=>new E('button');vm.createContext(sandbox);vm.runInContext(sidebar+'\n'+render+'\nglobalThis.render=render;',sandbox);sandbox.render();const list=f.find('.workspace-list'),signal=sidebarCtx.signal,oldSession=sessionCtx.signal;list.scrollTop=321;sandbox.location.hash='#/s/8';sandbox.render();assert.equal(mounted,1);assert.equal(cleaned,0);assert.equal(f.find('.workspace-list'),list);assert.equal(list.scrollTop,321);assert(oldSession.aborted);assert(!signal.aborted);await sidebarCtx.rpc({action:'git'});assert.equal(rpc.length,1,'sidebar RPC survives the previous session screen abort');assert(updates>=2);sandbox.state.cached=true;assert.throws(()=>sidebarCtx.rpc({action:'git'}),/Reconnect/);f.abort.abort();
 // Render the actual account screen through the app gate with an unavailable active host.
 sandbox.parseRoute=()=>({screen:'accounts'});sandbox.location.hash='#/accounts';sandbox.header=f.ctx.header;sandbox.M.screens.accounts=f.M.screens.accounts;f.ctx.connections=[{id:'pc',active:true,connection:{phase:'disconnected'}},{id:'other',name:'Online desktop',connection:{phase:'ready'}}];
 for(const cached of [false,true]){sandbox.state.snapshot=cached?{host:{display_name:'Cached PC'}}:null;sandbox.state.cached=cached;sandbox.state.connection={phase:'disconnected',message:'Reconnecting…'};transport.ready=false;sandbox.render();const choose=f.byText('Choose desktop');assert(choose&&!choose.hidden,'account host recovery survives missing/cached snapshots');choose.action();await f.sheets.at(-1)[1][0].action();assert.equal(f.ctx.selected,'other');assert.equal(f.routes.at(-1),'/accounts');}sandbox.abort.abort();
 console.log('PASS CLS tablet: session routes reuse sidebar DOM/scroll and independent RPC lifetime');
}

function pathLabelSnapshots(){
 const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8');
 const cases=[
  ["C:\\Users\\owen\\project", "windows", "project"],
  ["D:\\work\\project\\", null, "project"],
  ["C:/work/project/", null, "project"],
  ["\\\\server\\share\\project\\", null, "project"],
  ["\\\\server\\share\\", "windows", "share"],
  ["/home/owen/project/", null, "project"],
  ["/home/owen/literal\\name", "linux", "literal\\name"],
  ["/home/owen/literal\\name", null, "literal\\name"],
  ["relative\\project", "windows", "project"],
  ["relative\\project", null, "relative\\project"],
  ["C:\\", "windows", "C:"],
  ["/", "linux", ""],
  ["", null, ""]
 ];
 for(const [cwd,platform,basename] of cases){
  const f=fixture('home'),session={id:1,state:'Working',cwd},data={capabilities:{bots:true},host:{platform},sessions:[session]},sandbox={state:f.state,M:f.M};
  vm.createContext(sandbox);vm.runInContext(source.split('\n').find(l=>l.includes('function group(data)'))+'globalThis.groupSnapshot=group;',sandbox);
  for(const provided of [undefined,'Provided workspace label']){
   session.group_label=provided;sandbox.groupSnapshot(data);const label=provided||basename;
   assert.equal([...f.state.groups.values()][0].label,label||'Workspace',JSON.stringify({cwd,platform,provided}));
   f.ctx.connections=[{id:'pc',name:'PC',connection:{phase:'ready'},snapshot:data}];f.view.update();
   assert.equal(f.find('.recent-session-row').querySelector('strong').textContent,label||'Terminal #1',JSON.stringify({cwd,platform,provided}));
  }
  delete data.host;f.ctx.connections[0].platform=platform;session.group_label=undefined;f.view.update();assert.equal(f.find('.recent-session-row').querySelector('strong').textContent,basename||'Terminal #1','saved host platform is used for legacy snapshots');
  session.title='Provided session title';f.view.update();assert.equal(f.find('.recent-session-row').querySelector('strong').textContent,session.title);
  f.abort.abort();
 }
 console.log('PASS path label snapshots and actual Home module: drive, UNC, platform metadata, POSIX literal backslash, trailing/root/empty, provided workspace/session labels');
}

async function dropScreen(){
 const CHUNK=384*1024,size=CHUNK+10,id='a'.repeat(32),shared=[];let held=0,lost=true,removed=false;
 const f=fixture('drop',{globals:{crypto:{getRandomValues:b=>b.fill(7)},btoa:s=>Buffer.from(s,'binary').toString('base64'),atob:s=>Buffer.from(s,'base64').toString('binary'),Blob:class{constructor(parts){this.parts=parts;}},File:class{constructor(parts,name){this.parts=parts;this.name=name;}},navigator:{canShare:()=>true,share:async({files})=>{shared.push(...files);}}},rpc:req=>{
  if(req.action==='drop_list')return {items:removed?[]:[{id,name:'report.md',size:5,modified_unix_ms:0}],inbox:'/Users/me/Downloads/Krill Drop'};
  if(req.action==='drop_get')return {data:Buffer.from('hello').toString('base64'),size:5,eof:true};
  if(req.action==='drop_remove'){removed=true;return {removed:true};}
  if(req.offset===held)held+=Buffer.from(req.data,'base64').length;
  if(lost){lost=false;return Promise.reject(Error('Disconnected. The action was not resent.'));}
  return held===req.size?{received:held,saved:'/Users/me/Downloads/Krill Drop/사진.jpg'}:{received:held};}});
 await f.flush();assert(f.root.textContent.includes('/Users/me/Downloads/Krill Drop'));assert(f.root.textContent.includes('report.md'));
 const picker=f.all().find(n=>n.tagName==='INPUT'&&n.type==='file');picker.files=[{name:'사진.jpg',size,slice:(a,b)=>({arrayBuffer:async()=>new Uint8Array(b-a).buffer})}];emit(picker,'change');
 for(let i=0;i<5;i++){await f.flush();for(const [key,timer] of [...f.timers])if(timer.ms>=1000){f.timers.delete(key);timer.fn();}}
 const puts=f.calls.filter(c=>c.action==='drop_put');assert.deepEqual(puts.map(c=>c.offset),[0,0,CHUNK],'a lost reply is resent at the same offset and the PC answers what it holds');assert.equal(puts[0].upload,'07'.repeat(16));assert.equal(held,size);assert(f.root.textContent.includes('Saved to /Users/me/Downloads/Krill Drop/사진.jpg'));
 await f.byText('Download').action();await f.flush();assert.equal(shared.length,1);assert.equal(shared[0].name,'report.md');assert.equal(Buffer.from(await shared[0].parts[0].parts[0]).toString(),'hello');
 await f.byText('Remove').action();await f.flush();assert(f.calls.some(c=>c.action==='drop_remove'&&c.id===id));assert(f.root.textContent.includes('Nothing waiting'));f.abort.abort();
 const w=fixture('workspaces');assert(w.byText('Krill Drop').hidden,'older desktops without drop show no entry');w.state.snapshot.capabilities={drop:true};w.view.update();assert(!w.byText('Krill Drop').hidden);await w.byText('Krill Drop').action();assert.equal(w.routes.at(-1),'/drop');w.abort.abort();
 console.log('PASS Krill Drop screen: inbox path, chunked upload resumed after a lost reply, download shared as a file, remove, empty state and the capability-gated PC entry');
}
async function tabBarAndLayoutContracts(){
 const css=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.css'),'utf8');
 const mobileAppRule=css.match(/#mobile-app\{([^}]*)\}/)?.[1]||'';
 assert(/\bbottom:\s*0\b/.test(mobileAppRule),'#mobile-app anchors to bottom:0 instead of 100vh/100dvh');
 assert(!/100d?vh/.test(mobileAppRule),'#mobile-app must not use 100vh or 100dvh');
 assert(!css.includes('100dvh'),'app.css avoids 100dvh so overflow:hidden Android WebViews never inflate to 100lvh');
 assert(/\.home-list\{[^}]*padding-bottom:\s*calc\(140px\s*\+\s*env\(safe-area-inset-bottom\)\)/.test(css),'.home-list clears .home-fab and .tab-bar');
 assert(/\.workspace-bottom-bar\{[^}]*\bflex:\s*none\b/.test(css)&&/\.workspace-bottom-bar\{[^}]*\bflex-wrap:\s*nowrap\b/.test(css),'.workspace-bottom-bar stays flex:none and nowrap');
 const source=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8'),renderStart=source.indexOf(' function render(){'),renderEnd=source.indexOf(' // IME',renderStart);
 assert(renderStart>=0&&renderEnd>renderStart,'app.js render() slice markers must stay intact');
 const renderSrc=source.slice(renderStart,renderEnd);
 const makeSandbox=(routeFn,profilesFn,screens,loadScreenFn)=>{
  const f=fixture('home'),E=f.root.constructor;
  const root=new E('main'),banner=new E('div');
  f.doc.body.append(banner,root);
  const box={
   root,banner,document:f.doc,location:{hash:'#/accounts'},activeRouteScreen:'home',routeKey:'',
   state:{snapshot:null,cached:false,connection:{phase:'idle',message:'Not connected'},preferences:{}},
   hostGeneration:1,transport:{ready:false},screen:null,abort:null,install:f.ctx.install,
   endpoint:{profiles:profilesFn},routeAssets:{accounts:'screens/details.js',settings:'screens/settings.js',git:'screens/details.js'},
   M:{screens},AbortController,matchMedia:()=>({matches:false}),parseRoute:routeFn,loadScreen:loadScreenFn,
   el:(...v)=>new E(...v),header:title=>{const h=new E('header');h.append(new E('button'),new E('h1',title));return h;},
   button:(label,action,cls='')=>{const b=new E('button',label,cls);b.setAttribute('aria-label',label);b.action=action;return b;},
   t:s=>s,go(){},setRoute(){},back(){},backTo(){},icon:()=>new E('svg'),dot:()=>new E('span'),rollup:()=>'Unknown',
   groupRoute:()=>'/',linkRow:()=>new E('button'),sheet(){},closeSheet(){},longPress(){},savePreferences(){},
   rememberSession(){},resumeLastSession(){},scanQr(){},pasteLink(){},newWorkspace(){},sessionKey:()=>'k',
   selectHost(){},forgetHost(){},renameHost(){},hostRoute:()=>'/host/local',getConnections:()=>[],filteredConnections:()=>[],openRecent(){},
   reconnectHost(){},photo(){},toast(){},error(){},act:fn=>fn(),createSession(){},creating:()=>null,syncTablet(){},
   setDeviceFilter(){},deviceFilterPill:()=>new E('button'),syncDevicePills(){},syncTabBar(){},homeMode:()=>'bots',
   rpcForHost:()=>Promise.resolve({}),refreshHost:()=>Promise.resolve(),activateHost(){},
   tabletList:null,tabletAbort:null,tabletContext:null,tabletShell:null,tabletBody:null,tabletSidebar:null,
   tabletHost:null,tabletCollapsed:false,tabletCtxRef:null
  };
  vm.createContext(box);vm.runInContext(renderSrc+'\nglobalThis.render=render;',box);
  return {box,root,f,cleanup:()=>{box.abort?.abort();f.abort.abort();}};
 };
 // 1) Lazy-loading top-level tabs keeps .tab-bar mounted during loading and on Retry failure.
 let loadDeferred=deferred(),currentRoute={screen:'accounts'};
 const loading=makeSandbox(()=>currentRoute,()=>[{id:'pc'}],{},()=>loadDeferred.promise);
 loading.box.render();
 assert(loading.root.querySelector('.screen'),'.screen exists while loading accounts');
 assert(loading.root.querySelector('.detail-list'),'.detail-list guards snapshot() from clobbering loading screen');
 assert(loading.root.querySelector('.tab-bar'),'.tab-bar stays visible while loading accounts');
 assert.deepEqual(loading.root.querySelectorAll('.tab-bar-item').map(b=>b.getAttribute('aria-label')),['Bots','Projects','To Do','Me'],'Variant B 4-tab bottom bar order');
 assert.equal(loading.root.querySelectorAll('.tab-bar-item').find(b=>b.getAttribute('aria-current')==='page')?.getAttribute('aria-label'),'Me','/accounts compat highlights Me tab');
 loadDeferred.reject(Error('Network failed'));
 await loading.f.flush();
 assert(loading.root.querySelector('.tab-bar'),'.tab-bar remains visible on load error with Retry');
 assert(loading.root.querySelectorAll('button').some(b=>b.textContent==='Retry'));
 currentRoute={screen:'settings'};loading.box.location.hash='#/settings';loading.box.render();
 assert(loading.root.querySelector('.tab-bar'),'.tab-bar stays visible while loading settings');
 assert.equal(loading.root.querySelectorAll('.tab-bar-item').find(b=>b.getAttribute('aria-current')==='page')?.getAttribute('aria-label'),'Me');
 currentRoute={screen:'settings',subpage:'terminal'};loading.box.location.hash='#/settings/terminal';loading.box.render();
 assert.equal(loading.root.querySelector('.tab-bar'),null,'settings subpages omit .tab-bar even while loading');
 currentRoute={screen:'bots',botId:'b1',subpage:'b1'};loading.box.location.hash='#/bots/b1';loading.box.render();
 assert.equal(loading.root.querySelector('.tab-bar'),null,'bot detail omits .tab-bar');
 loading.cleanup();
 // 2) Rapid tab switching while loadScreen is pending suppresses stale double-render.
 const loads=[];let accountMounts=0;
 currentRoute={screen:'accounts'};
 const rapid=makeSandbox(()=>currentRoute,()=>[{id:'pc'}],{home:r=>{r.append(rapid.box.el('section','','home screen'));return {};}},()=>{const d=deferred();loads.push(d);return d.promise;});
 rapid.box.location.hash='#/accounts';rapid.box.render();
 currentRoute={screen:'home'};rapid.box.location.hash='#/';rapid.box.render();
 currentRoute={screen:'accounts'};rapid.box.location.hash='#/accounts';rapid.box.render();
 rapid.box.M.screens.accounts=r=>{accountMounts++;r.append(rapid.box.el('section','','accounts screen'));return {};};
 loads[0].resolve();loads[1].resolve();
 await rapid.f.flush();
 assert.equal(accountMounts,1,'aborted loadScreen callback does not double-mount accounts');
 assert.equal(rapid.root.querySelectorAll('.tab-bar').length,1);
 rapid.cleanup();
 // 3) Unpaired accounts route keeps .tab-bar and preserves .screen-fallback (no .detail-list).
 const unpaired=makeSandbox(()=>({screen:'accounts'}),()=>[],{accounts:r=>{r.append(unpaired.box.el('section','','accounts screen'));return {};}},()=>Promise.resolve());
 unpaired.box.render();
 assert(unpaired.root.querySelector('.screen'),'.screen wraps unpaired accounts fallback');
 assert(unpaired.root.querySelector('.tab-bar'),'.tab-bar remains visible on unpaired accounts route');
 assert.equal(unpaired.root.querySelector('.detail-list'),null,'unpaired fallback avoids .detail-list so snapshot() can replace it');
 assert(unpaired.root.querySelector('.screen-fallback'),'.screen-fallback stacks unpaired CTA cleanly above .tab-bar');
 unpaired.cleanup();
 console.log('PASS tab-bar and layout contracts: #mobile-app bottom:0, zero 100dvh, .home-list clearance, .tab-bar across loading/error/unpaired states');
}
async function botsScreens(){
 // 1. Security checks on bots.js + parseRoute coverage
 const botsJs=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/bots.js'),'utf8');
 for(const forbidden of ['innerHTML','crypto.subtle','crypto.randomUUID'])assert(!botsJs.includes(forbidden),`bots.js must not contain ${forbidden}`);
 const appJs=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8');
 const parseLine=appJs.split('\n').find(l=>l.includes('function parseRoute('));
 const routeBox={location:{hash:'#/bots'},hostRoute:()=>'/host/local',installRestoreHash:null};
 vm.createContext(routeBox);vm.runInContext(parseLine+'\nglobalThis.parseRoute=parseRoute;',routeBox);
 assert.deepEqual({...routeBox.parseRoute()},{screen:'bots',botId:undefined,subpage:undefined});
 routeBox.location.hash='#/bots/bot%201';
 assert.deepEqual({...routeBox.parseRoute()},{screen:'bots',botId:'bot 1',subpage:'bot 1'});
 routeBox.location.hash='#/up-next';
 assert.deepEqual({...routeBox.parseRoute()},{screen:'up-next',subpage:undefined});
 routeBox.location.hash='#/projects';
 assert.deepEqual({...routeBox.parseRoute()},{screen:'workspaces'});
 routeBox.location.hash='#/me';
 assert.deepEqual({...routeBox.parseRoute()},{screen:'settings',subpage:undefined});
 routeBox.location.hash='#/bots/%E0%A4%A';
 assert.throws(()=>routeBox.parseRoute(),/Invalid bot link/);

 // 2. Variant B Home: Bots default with conditional 1-line To Do banner + legacy fallback to Projects
 const hf=fixture('home');
 hf.ctx.connections=[{id:'one',active:true,name:'Desktop',connection:{phase:'ready'},snapshot:{capabilities:{bots:true},bots:{count:0,needs_you:0,pending_access:0},sessions:[],orchestration:{gates:[],questions:[],runs:[],tasks:[]}}}];
 hf.view.update();
 assert(!hf.find('.todo-summary-banner'),'0 pending items hide To Do summary banner on Bots home');
 hf.ctx.connections[0].snapshot.bots={count:2,needs_you:0,pending_access:0};
 hf.view.update();
 assert(!hf.find('.todo-summary-banner'),'bots without pending items hide To Do summary banner');
 hf.ctx.connections[0].snapshot.bots={count:2,needs_you:1,pending_access:1};
 hf.view.update();
 assert(hf.find('.todo-summary-banner'),'pending items show 1-line To Do summary banner on Bots home');
 assert(hf.find('.todo-summary-banner').textContent.includes('2 requests need your attention'));
 await hf.find('.todo-summary-banner').action();
 assert.equal(hf.routes.at(-1),'/up-next');
 // Legacy fallback: all ready hosts without capabilities.bots fall back to Projects (workspaces)
 hf.ctx.connections=[{id:'old',active:true,name:'Legacy PC',connection:{phase:'ready'},snapshot:{capabilities:{},sessions:[],orchestration:{}}}];
 assert.equal(hf.M.legacyNoBots(hf.ctx.connections),true);
 hf.view.update();
 assert(hf.find('.workspace-list'),'legacy hosts without bots capability fall back to Projects');
 hf.abort.abort();

 // 3. #/bots screen (A+C+D): avatar rail, one-tap decision deck, Open Chat keeps bot context, promoted Group Chat, Create Bot sheet, Access approve + redeem
 const botsData=[
  {id:'b1',slug:'triage',name:'Triage Bot',agent_id:'claude',description:'Triages PRs',paused:false,avatar_color:'#2563eb',schedules:[{id:'nightly',name:'Nightly triage',cron:'0 9 * * *',prompt:'Triage',enabled:true},{id:'weekly',name:'Weekly digest',cron:'0 17 * * 5',prompt:'Digest',enabled:false}]},
  {id:'b2',slug:'sleepy',name:'Paused Bot',agent_id:'codex',description:'Paused',paused:true,schedules:[]},
  {id:'b3',slug:'agybot',name:'Agy Bot',agent_id:'agy',model:'gemini-3.8-flash-low',effort:'high',description:'Antigravity helper',paused:false,schedules:[]},
 ];
 const itemData=[
  {uid:'item-1',owner_bot_id:'b1',title:'Review release',status:'needs_you',description:'Check hashes',done_means:'Signed',links:[{url:'https://example.com/pr/1'}],events:[{id:1,decisions:[{question:'Ship release?',options:['Approve','Hold'],recommended:0}]}],snoozed_until_ms:null},
 ];
 const accessData=[{token:'tok-1',bot_id:'b1',source:'telegram',sender_id:'77',sender_name:'Alice',status:'pending'}];
 const groupsData=[{id:'grp-1',name:'Eng Team',bot_ids:['b1'],description:'Sync'}];
 const suggestionsData=[{id:'sug-1',bot_id:'b1',title:'Check CI failures',why:'2 checks failed',ask:'Inspect main'}];
 const botSessionsData=[{session_id:99,bot_id:'b1',pid:4242,live:true,working:true,label:'Home',created_at_ms:1000}];
 let tabBarSynced=0;
 const bf=fixture('bots',{
  state:{snapshot:{capabilities:{bots:true},agents:[{id:'claude',name:'Claude Code'},{id:'agy',name:'Antigravity'}]}},
  refresh:()=>{bf.state.sessions.set(99,{id:99,groupKey:'cwd:/bots/b1'});bf.state.groups.set('cwd:/bots/b1',{key:'cwd:/bots/b1',sessions:[{id:99}]});},
  rpc:req=>{
   if(req.action==='bots.list')return {bots:botsData,truncated:false,next_cursor:null};
   if(req.action==='bots.items')return {items:itemData,truncated:false,next_cursor:null};
   if(req.action==='bots.access.list')return {requests:accessData,truncated:false,next_cursor:null};
   if(req.action==='bots.connectors.status')return {statuses:[{bot_id:'b1',source:'telegram',enabled:true,state:'connected'}]};
   if(req.action==='bots.groups.list')return {groups:groupsData,truncated:false,next_cursor:null};
   if(req.action==='bots.sessions.list')return {sessions:botSessionsData};
   if(req.action==='bots.groups.read')return {group:{group:groupsData[0],threads:[{thread_id:'t2',title:'Latest'},{thread_id:'t1',title:'Oldest'}],messages:[{sender_name:'Triage Bot',body:'Ready'}],active_run:false}};
   if(req.action==='bots.groups.new_thread')return {group:{group:groupsData[0],threads:[{thread_id:'t3',title:'New thread'},{thread_id:'t2',title:'Latest'},{thread_id:'t1',title:'Oldest'}],messages:[],active_run:false}};
   if(req.action==='bots.chat.open')return {bot_id:'b1',session_id:99,pid:4242};
   if(req.action==='bots.templates')return {templates:[{id:'pr-reviewer',name:'PR Reviewer',description:'Reviews PRs',mascot:'R'}]};
   if(req.action==='bots.suggestions.list')return {suggestions:suggestionsData};
   if(req.action==='bots.look_around')return {bot_id:'b1',suggestions:suggestionsData};
   return {};
  }
 });
 bf.ctx.syncTabBar=()=>{tabBarSynced++;};
 await bf.flush();
 assert(bf.root.textContent.includes('Triage Bot'));
 assert(bf.root.textContent.includes('Agy Bot')&&bf.root.textContent.includes('Antigravity'),'agy bot renders Antigravity label on Bots hub');
 assert(bf.root.textContent.includes('1 needs you'));
 // C: Top avatar rail renders bots + group + New Bot with numeric badge
 assert(bf.find('.bot-rail'),'avatar quick-switch rail renders on Bots hub');
 const railItems=bf.all().filter(n=>n.className&&n.className.includes('bot-rail-item'));
 assert(railItems.some(n=>n.dataset.botRailId==='b1'&&n.textContent.includes('1')),'bot rail item shows needs_you badge');
 assert(railItems.some(n=>n.dataset.botRailId==='b3'),'agy bot appears in avatar rail');
 assert(railItems.some(n=>n.dataset.groupRailId==='grp-1'),'group chat appears in avatar rail');
 // D: One-tap decision deck renders urgent Needs You item and syncs tab bar on answer
 const deck=bf.find('.bot-decision-deck');
 assert(deck&&deck.textContent.includes('Review release')&&deck.textContent.includes('Ship release?'),'decision deck shows urgent item');
 const deckApprove=deck.descendants().find(n=>n.tagName==='BUTTON'&&n.getAttribute('aria-label')==='Approve');
 await deckApprove.action();
 assert(bf.calls.some(c=>c.action==='bots.item.answer'&&c.item_id==='item-1'&&c.option_index===0));
 assert(tabBarSynced>0,'answering decision card syncs bottom To Do tab badge');
 // Open Chat keeps bot context inside #/bots/:id (does not jump out to /host/.../group/...)
 const chatButtons=bf.all().filter(b=>b.tagName==='BUTTON'&&b.getAttribute('aria-label')==='Open Chat');
 assert.equal(chatButtons.length,3);
 assert.equal(chatButtons[0].disabled,false);
 const pausedBtn=chatButtons.find(b=>b.disabled);
 assert(pausedBtn,'Paused bot disables Open Chat');
 await chatButtons[0].action();
 assert.equal(bf.routes.at(-1),'/bots/b1','Open Chat stays inside bot workspace instead of jumping to project session');

 const accessApprove=bf.find('.bots-access-card').descendants().find(b=>b.tagName==='BUTTON'&&b.getAttribute('aria-label')==='Approve');
 await accessApprove.action();
 assert(bf.calls.some(c=>c.action==='bots.access.decide'&&c.token==='tok-1'&&c.approve===true));
 const codeInput=bf.find('.bots-redeem-form').querySelector('input');
 codeInput.value='PAIR-99';
 await bf.byText('Redeem').action();
 assert(bf.calls.some(c=>c.action==='bots.access.redeem'&&c.code==='PAIR-99'));

 // Promoted Group Chat via avatar rail 1-tap
 const grpRailBtn=bf.all().find(n=>n.dataset?.groupRailId==='grp-1');
 await grpRailBtn.action();
 await bf.flush();
 assert(bf.find('.bot-group-promoted'),'tapping group in rail opens promoted Group Chat view');
 assert(bf.root.textContent.includes('Ready'));
 const grpArea=bf.find('.bot-group-panel').querySelector('textarea');
 grpArea.value='@triage status?';
 await bf.find('.bot-group-panel').querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Send').action();
 assert(bf.calls.some(c=>c.action==='bots.groups.send'&&c.group_id==='grp-1'&&c.thread_id==='t2'&&c.text==='@triage status?'));
 await bf.find('.bot-group-panel').querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='+ Thread').action();
 const grpAreaAfter=bf.find('.bot-group-panel').querySelector('textarea');
 grpAreaAfter.value='@triage in new thread';
 await bf.find('.bot-group-panel').querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Send').action();
 assert(bf.calls.some(c=>c.action==='bots.groups.send'&&c.group_id==='grp-1'&&c.thread_id==='t3'&&c.text==='@triage in new thread'));
 await bf.byText('Back to bots').action();
 assert(!bf.find('.bot-group-promoted'),'Back to bots closes promoted group chat');

 await bf.byText('New Bot').action();
 const sheet=bf.sheets.at(-1);
 assert.equal(sheet[0],'New Bot');
 bf.ctx.closeSheet=()=>{};
 const bodyNode=bf.ctx.el('div');
 sheet[3](bodyNode);
 await bf.flush();
 bodyNode.querySelector('input').value='Release Bot';
 await bodyNode.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Create Bot').action();
 assert(bf.calls.some(c=>c.action==='bots.create'&&c.name==='Release Bot'));
 bf.abort.abort();

 // 4. #/up-next and #/bots/:id: decision answer, snooze FieldPatch, look around, soul/memory
 const uf=fixture('up-next',{
  state:{snapshot:{capabilities:{bots:true}}},
  rpc:req=>{
   if(req.action==='bots.list')return {bots:botsData};
   if(req.action==='bots.items')return {items:itemData};
   if(req.action==='bots.access.list')return {requests:[]};
   if(req.action==='bots.suggestions.list')return {suggestions:suggestionsData};
   return {};
  }
 });
 await uf.flush();
 assert(uf.root.textContent.includes('Ship release?'));
 await uf.byText('Approve').action();
 assert(uf.calls.some(c=>c.action==='bots.item.answer'&&c.item_id==='item-1'&&c.option_index===0));
 await uf.byText('Snooze 4h').action();
 assert(uf.calls.some(c=>c.action==='bots.item.update'&&c.item_id==='item-1'&&typeof c.snoozed_until_ms?.set==='number'));
 itemData[0].snoozed_until_ms=Date.now()+100000;
 uf.view.update();
 await uf.byText('Unsnooze').action();
 assert(uf.calls.some(c=>c.action==='bots.item.update'&&c.snoozed_until_ms==='clear'));
 await uf.byText('Done').action();
 assert(uf.calls.some(c=>c.action==='bots.item.update'&&c.status==='done'));
 await uf.byText('Dismiss').action();
 assert(uf.calls.some(c=>c.action==='bots.suggestions.dismiss'&&c.suggestion_id==='sug-1'));
 uf.abort.abort();

 // 4b. Unified To Do hub aggregates waiting sessions + orchestration gates/questions + bot items across hosts, and shows All caught up when empty
 const uni=fixture('up-next',{
  state:{snapshot:{capabilities:{bots:true}}},
  rpc:req=>{
   if(req.action==='bots.list')return {bots:botsData};
   if(req.action==='bots.items')return {items:[]};
   if(req.action==='bots.access.list')return {requests:[]};
   if(req.action==='bots.suggestions.list')return {suggestions:[]};
   return {};
  }
 });
 uni.ctx.connections=[
  {id:'h1',active:true,name:'Mac Mini',connection:{phase:'ready'},snapshot:{capabilities:{bots:true},sessions:[{id:5,state:'Waiting',title:'Deploy check',worktree_key:'w1'}],orchestration:{gates:[{id:'g1',status:'pending',question:'Ship v2?',options:['Approve','Deny']}],questions:[],runs:[],tasks:[]}}},
  {id:'h2',name:'MacBook',connection:{phase:'ready'},snapshot:{capabilities:{bots:true},sessions:[],orchestration:{gates:[],questions:[],runs:[],tasks:[]}}}
 ];
 uni.view.update();await uni.flush();
 assert(uni.root.textContent.includes('Waiting sessions'));
 assert(uni.root.textContent.includes('Deploy check'));
 assert(uni.root.textContent.includes('Ship v2?'));
 assert.equal(uni.M.todoCounts(uni.ctx.connections).total,2);
 await uni.byText('Approve').action();
 assert(uni.calls.some(c=>c.action==='gate_reply'&&c.id==='g1'&&c.decision==='Approve'));
 uni.ctx.connections[0].snapshot.sessions=[];
 uni.ctx.connections[0].snapshot.orchestration.gates=[];
 uni.view.update();await uni.flush();
 assert(uni.root.textContent.includes('All caught up'));
 uni.abort.abort();

 // 5. #/bots/:id Bot Workspace (A+C+D): 4 segment tabs, embedded chat, worker strip, collapsible items, automations run/toggle, settings (agy + pause + SOUL/MEMORY)
 let mountedChatSessions=[];
 let unmountedChats=0;
 const df=fixture('bots',{
  route:{botId:'b1'},
  state:{snapshot:{capabilities:{bots:true}}},
  refresh:()=>{df.state.sessions.set(99,{id:99,groupKey:'cwd:/bots/b1'});df.state.groups.set('cwd:/bots/b1',{key:'cwd:/bots/b1',sessions:[{id:99}]});},
  rpc:req=>{
   if(req.action==='bots.list')return {bots:botsData};
   if(req.action==='bots.items')return {items:itemData};
   if(req.action==='bots.connectors.status')return {statuses:[{bot_id:'b1',source:'telegram',state:'connected'}]};
   if(req.action==='bots.suggestions.list')return {suggestions:suggestionsData};
   if(req.action==='bots.access.list')return {requests:[]};
   if(req.action==='bots.sessions.list')return {sessions:botSessionsData};
   if(req.action==='bots.groups.list')return {groups:groupsData};
   if(req.action==='bots.upcoming')return {look_around_last_ms:Date.now()-60000,schedules:[{id:'nightly',running:false,next_ms:Date.now()+3600000}]};
   if(req.action==='bots.soul.get')return {bot_id:'b1',content:'# Soul'};
   if(req.action==='bots.memory.get')return {bot_id:'b1',content:'- Fact'};
   if(req.action==='bots.memory.update')return {content:'- Fact\n- More',restored_facts:[]};
   if(req.action==='bots.look_around')return {suggestions:[]};
   if(req.action==='bots.schedule.run')return {started:true};
   if(req.action==='bots.schedule.toggle'){botsData[0].schedules[0].enabled=req.enabled;return {bot:botsData[0]};}
   if(req.action==='bots.sessions.stop')return {stopped:true};
   if(req.action==='bots.models')return {models:req.agent_id==='agy'?[{id:'gemini-3.8-flash-low',label:'Gemini 3.8 Flash (Low)'}]:[{id:'opus',label:'Opus'}]};
   if(req.action==='bots.update'){botsData[0]={...botsData[0],paused:req.paused??botsData[0].paused,agent_id:req.agent_id||botsData[0].agent_id,model:req.clear_model?null:(req.model??botsData[0].model),effort:req.clear_effort?null:(req.effort??botsData[0].effort)};return {bot:botsData[0]};}
   return {};
  }
 });
 df.M.chat={mount:(node,opts)=>{mountedChatSessions.push(opts.sessionId);node.append(df.ctx.el('div',`Embedded chat #${opts.sessionId}`,'chat-view'));return {setVisible(){},update(){},unmount(){unmountedChats++;}};}};
 df.view.update();
 await df.flush();
 // Default Chat tab mounts embedded chat + shows avatar rail + worker strip + Terminal switch
 const segTabs=df.all().filter(n=>n.dataset?.botTab);
 assert.deepEqual(segTabs.map(n=>n.dataset.botTab),['chat','work','automations','settings'],'4 segment tabs in Bot Workspace');
 assert(df.find('.bot-rail'),'avatar rail renders inside Bot Workspace for 1-tap switching');
 assert(mountedChatSessions.includes(99),'M.chat.mount embeds live bot session inside Chat tab');
 assert(df.find('.bot-worker-strip'),'worker strip shows active bot session');
 await df.find('.bot-worker-strip').descendants().find(b=>b.tagName==='BUTTON'&&b.getAttribute('aria-label')==='Stop').action();
 assert(df.calls.some(c=>c.action==='bots.sessions.stop'&&c.bot_id==='b1'&&c.session_id===99));

 // Work tab: collapsible item cards + Look Around + External reply
 await segTabs.find(n=>n.dataset.botTab==='work').action();
 const itemDetails=df.find('.bot-item-details');
 assert(itemDetails&&itemDetails.hidden,'item details start collapsed in Work tab to avoid mis-taps');
 await df.byText('Details').action();
 assert(!df.find('.bot-item-details').hidden,'tapping Details expands item details');
 await df.byText('Look Around').action();
 assert(df.calls.some(c=>c.action==='bots.look_around'&&c.bot_id==='b1'));
 const replyBox=df.find('.bot-reply-card').querySelector('textarea');
 replyBox.value='Ack';
 await df.byText('Send reply').action();
 assert(df.calls.some(c=>c.action==='bots.reply'&&c.bot_id==='b1'&&c.text==='Ack'));

 // Automations tab: lists all schedules (active + paused) and supports Run now + Pause/Enable toggle
 await segTabs.find(n=>n.dataset.botTab==='automations').action();
 assert(df.root.textContent.includes('Nightly triage')&&df.root.textContent.includes('Weekly digest'),'Automations tab shows all schedules, not just failures');
 const autoRunBtns=df.all().filter(b=>b.tagName==='BUTTON'&&b.getAttribute('aria-label')==='Run now');
 assert(autoRunBtns.length>=2,'Look Around + schedules have Run now buttons');
 await autoRunBtns[1].action();
 assert(df.calls.some(c=>c.action==='bots.schedule.run'&&c.bot_id==='b1'&&c.schedule_id==='nightly'));
 await df.byText('Pause').action();
 assert(df.calls.some(c=>c.action==='bots.schedule.toggle'&&c.bot_id==='b1'&&c.schedule_id==='nightly'&&c.enabled===false));

 // Settings tab: Pause/Resume, agy provider switch, SOUL & MEMORY auto-loaded
 await segTabs.find(n=>n.dataset.botTab==='settings').action();
 await df.flush();
 assert(df.root.textContent.includes('SOUL.md'),'Settings tab loads SOUL.md');
 await df.byText('Save Soul').action();
 assert(df.calls.some(c=>c.action==='bots.soul.set'&&c.content==='# Soul'));
 await df.byText('Agent').action();
 assert.equal(df.sheets.at(-1)[0],'Agent');
 const agentBody=df.ctx.el('div');
 df.sheets.at(-1)[3](agentBody);
 await df.flush();
 const selects=agentBody.querySelectorAll('select');
 assert.equal(selects.length,3);
 selects[0].value='agy';
 emit(selects[0],'change');
 await df.flush();
 assert(df.calls.some(c=>c.action==='bots.models'&&c.agent_id==='agy'));
 selects[1].value='gemini-3.8-flash-low';
 const saveBtn=agentBody.descendants().find(n=>n.tagName==='BUTTON'&&n.getAttribute('aria-label')==='Save');
 await saveBtn.action();
 await df.flush();
 assert(df.calls.some(c=>c.action==='bots.update'&&c.bot_id==='b1'&&c.agent_id==='agy'&&c.model==='gemini-3.8-flash-low'));
 assert(df.root.textContent.includes('Antigravity'),'Settings tab displays Antigravity for agy bot');
 df.view.cleanup?.();
 assert(unmountedChats>0,'leaving Bot Workspace unmounts embedded chat cleanly');
 df.abort.abort();

 // 6. Refused bot chats on mobile: Start chat refusal, agent switch Home Chat (#46862), and failed automation decision card (#46892).
 const tooOldRefusal={code:'cli_too_old',agent:'hermes',installed_version:'0.20.5',minimum_version:'0.23.0'};
 const rf=fixture('bots',{
  route:{botId:'b1'},
  state:{snapshot:{capabilities:{bots:true}}},
  rpc:req=>{
   if(req.action==='bots.list')return {bots:[{...botsData[0],agent_id:'claude',schedules:[{id:'nightly',name:'Nightly triage',cron:'0 9 * * *',prompt:'Triage',enabled:true}]}]};
   if(req.action==='bots.items')return {items:[]};
   if(req.action==='bots.connectors.status')return {statuses:[]};
   if(req.action==='bots.suggestions.list')return {suggestions:[]};
   if(req.action==='bots.access.list')return {requests:[]};
   if(req.action==='bots.sessions.list')return {sessions:[]};
   if(req.action==='bots.groups.list')return {groups:[]};
   if(req.action==='bots.upcoming')return {schedules:[{id:'nightly',running:false,last_error:'Your Hermes CLI on this computer is 0.20.5. Chats need 0.23.0 or newer.',last_refusal:tooOldRefusal}]};
   if(req.action==='bots.models')return {models:[]};
   if(req.action==='bots.update')return {bot:{...botsData[0],agent_id:req.agent_id||'hermes'}};
   if(req.action==='bots.chat.open'){const err=Error('Your Hermes CLI on this computer is 0.20.5. Chats need 0.23.0 or newer.');err.data={refusal:tooOldRefusal};throw err;}
   return {};
  }
 });
 const toasts=[];
 rf.ctx.toast=(m)=>toasts.push(m);
 await rf.flush();
 // Failed automation card surfaces immediately on Chat tab decision deck with phone-side refusal text (no "this computer").
 const runRow=rf.find('.bot-automation-run-row');
 assert(runRow,'failed automation run card must render on mobile');
 assert(runRow.textContent.includes('Hermes')&&runRow.textContent.includes('0.20.5')&&runRow.textContent.includes('0.23.0'),runRow.textContent);
 assert(!runRow.textContent.includes('this computer'),runRow.textContent);
 // Explicit Start chat tap rejects with worded refusal without "this computer".
 let openErr=null;
 try{await rf.byText('Start chat').action();}catch(e){openErr=e;}
 assert(openErr&&openErr.message.includes('Hermes')&&openErr.message.includes('0.20.5')&&openErr.message.includes('0.23.0'),String(openErr));
 assert(!openErr.message.includes('this computer'),openErr.message);
 // Switching agent in Settings -> Agent sheet ensures Home Chat and toasts why it was refused (#46862).
 await rf.all().find(n=>n.dataset?.botTab==='settings').action();
 await rf.byText('Agent').action();
 const switchBody=rf.ctx.el('div');
 rf.sheets.at(-1)[3](switchBody);
 await rf.flush();
 const swSelects=switchBody.querySelectorAll('select');
 swSelects[0].value='agy';
 emit(swSelects[0],'change');
 await rf.flush();
 const swSave=switchBody.descendants().find(n=>n.tagName==='BUTTON'&&n.getAttribute('aria-label')==='Save');
 await swSave.action();
 await rf.flush();
 const lastToast=toasts.at(-1)||'';
 assert(lastToast.includes('Hermes')&&lastToast.includes('0.20.5')&&lastToast.includes('0.23.0'),lastToast);
 assert(!lastToast.includes('this computer'),lastToast);
 rf.abort.abort();

 // 7. Korean i18n coverage for all new Bot UX A+C+D copy
 const koBox=vm.createContext({navigator:{language:'ko'}});
 vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/i18n.js'),'utf8'),koBox);
 const K=koBox.KrillMobile.i18n;
 for(const key of ['Quick switch','Back to bots','New Chat','Home Chat','Work','Start chat','Active workers','Run now','Pause','Resume','Enable','No scheduled automations configured.','Bot status','Provider & model','Failed automation','Access request','Needs attention','Open in terminal']){
  assert.notEqual(K.t(key),key,`missing ko translation for "${key}"`);
 }
 assert.equal(K.t('{0} active',2),'2개 실행 중');
 console.log('PASS mobile bots screens (A+C+D): rail, decision deck, 4-tab workspace, embedded chat, worker strip, collapsible items, automations, agy, promoted groups, refusals, ko i18n, no innerHTML');
}
async function mobileAuditWave2Regressions(){
 const assetRoot=path.join(__dirname,'../src/mobile/assets');
 const appSrc=fs.readFileSync(path.join(assetRoot,'app.js'),'utf8');
 const clientSrc=fs.readFileSync(path.join(assetRoot,'client.js'),'utf8');
 const chatSrc=fs.readFileSync(path.join(assetRoot,'screens/chat.js'),'utf8');
 const cssSrc=fs.readFileSync(path.join(assetRoot,'app.css'),'utf8');
 const i18nSrc=fs.readFileSync(path.join(assetRoot,'i18n.js'),'utf8');

 // M-01 & M-09: app.js ctx RPC/refresh/createSession wrappers return rejected Promises (never sync throw) on stale/cached targets.
 {
  const start=appSrc.indexOf('const screenTransport=transport'),end=appSrc.indexOf(',creating:()=>creating};',start)+',creating:()=>creating};'.length;
  const abort=new AbortController(),state={cached:false};
  let transport={ready:true,rpc:()=>Promise.resolve({ok:true}),rpcLatest:()=>Promise.resolve({ok:true}),refresh:()=>Promise.resolve({})};
  const box=vm.createContext({
   state,transport,hostGeneration:1,abort,pool:null,
   el:()=>{},button:()=>{},icon:()=>{},go:()=>{},setRoute:()=>{},back:()=>{},backTo:()=>{},t:x=>x,
   install:{},route:{},header:()=>{},dot:()=>{},rollup:()=>{},groupRoute:()=>{},linkRow:()=>{},
   sheet:()=>{},closeSheet:()=>{},longPress:()=>{},savePreferences:()=>{},rememberSession:()=>{},
   resumeLastSession:()=>{},scanQr:()=>{},pasteLink:()=>{},newWorkspace:()=>{},sessionKey:()=>{},
   endpoint:{profiles:()=>[]},selectHost:()=>{},forgetHost:()=>{},renameHost:()=>{},hostRoute:()=>{},
   getConnections:()=>[],openRecent:()=>{},reconnectHost:()=>{},photo:()=>{},toast:()=>{},error:()=>{},
   act:fn=>fn(),render:()=>{},createSession:()=>Promise.resolve({}),snapshot:()=>{},activateEntrySilent:()=>{},
  });
  vm.runInContext(appSrc.slice(start,end)+'\nglobalThis.testCtx=ctx;',box);
  box.hostGeneration=2;
  for(const [name,call] of [
   ['rpc',()=>box.testCtx.rpc({action:'chat_unsubscribe',session:1})],
   ['rpcForHost',()=>box.testCtx.rpcForHost(null,{action:'bots.list'})],
   ['rpcLatest',()=>box.testCtx.rpcLatest('k',{action:'git'})],
   ['refresh',()=>box.testCtx.refresh()],
   ['refreshHost',()=>box.testCtx.refreshHost(null)],
   ['createSession',()=>box.testCtx.createSession({key:'g'},'claude',{action:'create_terminal'})],
  ]){
   let p;
   assert.doesNotThrow(()=>{p=call();},`${name} must not throw synchronously on target change`);
   await assert.rejects(p,/Screen target changed/,`${name} must reject asynchronously`);
  }
 }

 // M-01 & M-06: chat.js unmount survives sync-throwing ctx.rpc & clears mounts/listeners; Stop unlocks on interrupt error or interrupted:false.
 {
  const f=fixture('home');
  const E=f.root.constructor;
  const addedListeners=[],removedListeners=[];
  const chatDoc={
   activeElement:f.doc.body,body:f.doc.body,hidden:false,visibilityState:'visible',
   documentElement:new E('html'),
   createElement:tag=>new E(tag),
   createElementNS:(_,tag)=>new E(tag),
   addEventListener:(ev,fn)=>{addedListeners.push([ev,fn]);},
   removeEventListener:(ev,fn)=>{removedListeners.push([ev,fn]);},
   querySelector:()=>null,querySelectorAll:()=>[],
  };
  let rpcImpl=async req=>{
   if(req.action==='chat_poll')return {items:[],next_offset:10,agent:'claude',bound:true,has_more:false};
   if(req.action==='chat_subscribe')return {subscribed:true};
   if(req.action==='chat_unsubscribe')return {unsubscribed:true};
   if(req.action==='chat_composer')return {};
   return {};
  };
  const chatCalls=[];
  const chatState={
   snapshot:{capabilities:{chat_push:true,chat_interrupt:true,chat_tail:true},sessions:[{id:7,state:'Working',agent:'claude',state_since_unix_ns:'100'}]},
   connection:{phase:'ready',generation:1},cached:false,
   sessions:new Map([[7,{id:7,state:'Working',agent:'claude',state_since_unix_ns:'100'}]]),
   groups:new Map(),drafts:new Map(),preferences:{},chatCache:new Map(),ui:{},
  };
  const chatErrors=[];
  const chatCtx={
   ...f.ctx,state:chatState,
   act:fn=>Promise.resolve().then(fn).catch(e=>chatErrors.push(e)),
   error:e=>chatErrors.push(e),
   rpc:req=>{chatCalls.push(req);return rpcImpl(req);},
   refresh:async()=>{},
   sessionKey:id=>`s:${id}`,
  };
  const chatSandbox=vm.createContext({
   document:chatDoc,window:globalThis,navigator:{clipboard:{writeText:async()=>{}}},
   AbortController,Date,console,Math,Map,Set,Array,Object,JSON,Number,String,Boolean,Promise,Error,RegExp,
   setTimeout:()=>0,
   clearTimeout:()=>{},requestAnimationFrame:fn=>fn(),
   KrillMobile:{agentName:a=>a,agentIcon:()=>new E('svg')},
  });
  vm.runInContext(chatSrc,chatSandbox);
  const host=new E('div');chatDoc.body.append(host);
  const handle=chatSandbox.KrillMobile.chat.mount(host,{ctx:chatCtx,sessionId:7,active:()=>true});
  await f.flush();
  assert(chatCalls.some(c=>c.action==='chat_subscribe'&&c.session===7),'chat_subscribe sent');
  const entry=chatState.chatCache.get(7);
  assert.equal(entry.mounts,1);

  // M-06 case 1: interrupt RPC fails -> entry.stopping and stopCooldownUntil reset immediately.
  const sendBtn=host.querySelector('.composer-send');
  assert(sendBtn,'send/stop button exists');
  rpcImpl=async req=>{if(req.action==='interrupt')throw Error('network lost');return {};};
  const clickEv=new Event('click',{cancelable:true});clickEv.detail=1;
  sendBtn.dispatchEvent(clickEv);
  await f.flush();
  assert.equal(entry.stopping,false,'M-06: stopping unlocked after interrupt RPC failure');
  assert.equal(entry.stopCooldownUntil,0,'M-06: stop cooldown cleared on interrupt RPC failure');
  assert.equal(sendBtn.disabled,false,'M-06: stop button re-enabled after interrupt failure');

  // M-06 case 2: interrupt returns {interrupted:false} -> entry.stopping unlocked.
  rpcImpl=async req=>{if(req.action==='interrupt')return {interrupted:false};return {};};
  const clickEv2=new Event('click',{cancelable:true});clickEv2.detail=1;
  sendBtn.dispatchEvent(clickEv2);
  await f.flush();
  assert.equal(entry.stopping,false,'M-06: stopping unlocked when daemon reports interrupted:false');

  // M-01: unmount() survives a synchronously throwing ctx.rpc (e.g., inside clearHost) and runs full cleanup.
  chatCtx.rpc=()=>{throw Error('Screen target changed');};
  assert.doesNotThrow(()=>handle.unmount(),'M-01: chat unmount must not throw when ctx.rpc throws synchronously');
  assert.equal(entry.mounts,0,'M-01: entry.mounts decremented despite chat_unsubscribe failure');
  assert(removedListeners.some(([ev])=>ev==='visibilitychange'),'M-01: visibilitychange listener removed');
  assert(removedListeners.some(([ev])=>ev==='keydown'),'M-01: keydown listener removed');
  f.abort.abort();
 }

 // M-02: setDeviceFilter() calls render() when transport swaps, and workspaces.js uses createSessionForHost.
 {
  const fnLine=appSrc.split('\n').find(l=>l.includes('function setDeviceFilter('));
  const tA={id:'tA'},tB={id:'tB'};
  let renders=0,updates=0,pills=0,tabBars=0;
  const box=vm.createContext({
   state:{ui:{deviceFilter:'all'}},transport:tA,
   pool:{get:id=>id==='b'?{id:'b',transport:tB}:id==='a'?{id:'a',transport:tA}:null},
   activateEntrySilent:e=>{box.transport=e.transport;},
   render:()=>{renders++;},syncDevicePills:()=>{pills++;},syncTabBar:()=>{tabBars++;},
   screen:{update:()=>{updates++;}},
  });
  vm.runInContext(fnLine+'\nglobalThis.setDeviceFilter=setDeviceFilter;',box);
  box.setDeviceFilter('a');
  assert.equal(renders,0,'same transport skips full render');assert.equal(updates,1);
  box.setDeviceFilter('b');
  assert.equal(renders,1,'M-02: switching to another host transport triggers render() to rebind screen ctx');
  assert.equal(updates,1,'M-02: render() replaces stale screen.update()');
  box.setDeviceFilter('all');
  assert.equal(renders,1,'all filter preserves current transport without full remount');assert.equal(updates,2);

  // workspaces.js passes hostId to ctx.createSessionForHost when starting a session on a sessionless workspace.
  const wf=fixture('workspaces');
  const createdForHost=[];
  wf.ctx.createSessionForHost=(hostId,group,agent,req)=>{createdForHost.push({hostId,workspace:group.key,agent,req});return Promise.resolve();};
  const hB={id:'host-b',name:'MacBook',active:true,connection:{phase:'ready'},cached:false,snapshot:{capabilities:{workspace_create:true},agents:[{id:'claude',name:'Claude'}],repositories:[{key:'r1',label:'Repo'}],workspaces:[{key:'w-empty',repo_key:'r1',label:'Empty',path:'/tmp/e'}],sessions:[]}};
  wf.ctx.connections=[hB];
  wf.state.groups=new Map([['w-empty',{key:'w-empty',label:'Empty',path:'/tmp/e',repoKey:'r1',sessions:[]}]]);
  wf.view.update();
  const row=wf.all().find(n=>n.dataset.group==='w-empty');
  assert(row,'sessionless workspace row rendered');
  await row.action();
  const sheetBody=wf.ctx.el('div');
  wf.sheets.at(-1)[3](sheetBody);
  await sheetBody.querySelector('.new-tab-tile').action();
  assert.equal(createdForHost.length,1);
  assert.equal(createdForHost[0].hostId,'host-b');
  assert.equal(createdForHost[0].workspace,'w-empty');
  wf.abort.abort();
 }

 // M-03 & M-05: client.js pool forwards onChatPush to active handler; isRead covers chat_composer + 13 bots.* read actions.
 {
  const cBox=vm.createContext({
   TextEncoder,TextDecoder,Uint8Array,DataView,URL,URLSearchParams,AbortController,Map,Set,Promise,Error,Date,Math,JSON,Number,String,Array,Object,RegExp,
   setTimeout,clearTimeout,crypto:{getRandomValues:b=>b.fill(1)},
   MkCrypto:{enc:new TextEncoder(),dec:new TextDecoder(),hex:()=>'00',unhex:()=>new Uint8Array(32),sha256:()=>new Uint8Array(32),concat:(...a)=>new Uint8Array(0),Channel:class{}},
  });
  vm.runInContext(clientSrc,cBox);
  const captured=[],received=[];
  const endpoint={
   profiles:()=>[{id:'h1',name:'One',active:true}],
   forHost:()=>({credentials:{device_id:'d1',credential:'c1'},describeHost(){}}),
  };
  const pool=cBox.KrillMobile.transport.pool({
   endpoint,
   onChatPush:(entry,payload)=>received.push({id:entry.id,payload}),
   create:opts=>{captured.push(opts);return {connect(){},close(){},resume(){},updateBusy:false};},
  });
  pool.sync();
  assert.equal(captured.length,1);
  assert.equal(typeof captured[0].onChatPush,'function','M-03: pool passes onChatPush to transport.create');
  captured[0].onChatPush({push:'chat_delta',session:7,items:[]});
  assert.equal(received.length,1);
  assert.equal(received[0].id,'h1');
  assert.equal(received[0].payload.push,'chat_delta');
  pool.close();
  captured[0].onChatPush({push:'chat_stale',session:7});
  assert.equal(received.length,1,'M-03: closed pool ignores late chat pushes');
  assert(appSrc.includes('onChatPush:(entry,p)=>{if(entry.transport===transport&&entry.id===activeView?.id)M.chat?.onPush?.(p);}'),'M-03: app.js wires pool onChatPush to M.chat.onPush for active host only');

  // M-05: verify isRead() via transport.rpcLatest (closed transport rejects reads immediately without 20s reconnect wait).
  const tr=cBox.KrillMobile.transport.create({credentials:{device_id:'d1',credential:'c1'},url:'wss://example.test/ws',warmTerminal:false});
  tr.close();
  const readActions=['chat_composer','bots.list','bots.templates','bots.models','bots.upcoming','bots.items','bots.access.list','bots.connectors.status','bots.groups.list','bots.groups.read','bots.suggestions.list','bots.avatar.read','bots.soul.get','bots.memory.get'];
  for(const action of readActions){
   const p=tr.rpcLatest('k:'+action,{action});
   let err=null;try{await p;}catch(e){err=e;}
   assert(err&&!/Only reads can be coalesced/.test(err.message),`M-05: ${action} must be treated as a read action, got: ${err?.message}`);
  }
  await assert.rejects(()=>tr.rpcLatest('mut',{action:'bots.update'}),/Only reads can be coalesced/,'M-05: mutations remain non-read');
 }

 // M-04 & M-08: multi-host bots.groups.create, bots.access.redeem, and multi-host cursor pagination (bots.list & bots.items).
 {
  const hostCalls=[];
  const h1={id:'h1',name:'Mac Mini',active:true,connection:{phase:'ready'},cached:false,snapshot:{capabilities:{bots:true},agents:[{id:'claude',name:'Claude'}]}};
  const h2={id:'h2',name:'MacBook',active:false,connection:{phase:'ready'},cached:false,snapshot:{capabilities:{bots:true},agents:[{id:'codex',name:'Codex'}]}};
  const bf=fixture('bots',{state:{snapshot:{capabilities:{bots:true}}}});
  bf.ctx.connections=[h1,h2];
  bf.ctx.rpcForHost=async(hid,req)=>{
   hostCalls.push({hid,req});
   if(req.action==='bots.list'){
    if(hid==='h1')return req.cursor?{bots:[{id:'b1-p2',slug:'b1p2',name:'Mini Bot 2',agent_id:'claude'}],next_cursor:null}:{bots:[{id:'b1',slug:'b1',name:'Mini Bot 1',agent_id:'claude'}],next_cursor:'c-h1'};
    if(hid==='h2')return req.cursor?{bots:[{id:'b2-p2',slug:'b2p2',name:'Book Bot 2',agent_id:'codex'}],next_cursor:null}:{bots:[{id:'b2',slug:'b2',name:'Book Bot 1',agent_id:'codex'}],next_cursor:'c-h2'};
   }
   if(req.action==='bots.items')return {items:[]};
   if(req.action==='bots.access.list')return {requests:[]};
   if(req.action==='bots.connectors.status')return {statuses:[]};
   if(req.action==='bots.groups.list')return {groups:[]};
   return {};
  };
  bf.view.update();
  await bf.flush();
  assert(bf.root.textContent.includes('Mini Bot 1')&&bf.root.textContent.includes('Book Bot 1'));

  // M-08 bots.list: Load more drains h1 cursor, then keeps Load more to drain h2 cursor.
  assert(bf.byText('Load more'),'M-08: Load more shown when hosts have cursors');
  await bf.byText('Load more').action();
  await bf.flush();
  assert(bf.root.textContent.includes('Mini Bot 2'),'M-08: h1 page 2 loaded');
  assert(bf.byText('Load more'),'M-08: Load more remains visible for h2 cursor');
  await bf.byText('Load more').action();
  await bf.flush();
  assert(bf.root.textContent.includes('Book Bot 2'),'M-08: h2 page 2 loaded');
  assert(!bf.byText('Load more'),'M-08: Load more hides only after all host cursors drain');

  // M-04 groups.create: host selector filters bots to chosen host and calls rpcForHost(hid).
  await bf.all().find(n=>n.dataset?.hubMode==='groups').action();
  await bf.byText('+ New group').action();
  const grpBody=bf.ctx.el('div');
  bf.sheets.at(-1)[3](grpBody);
  const grpHostSel=grpBody.querySelector('select');
  assert(grpHostSel,'M-04: group sheet shows desktop selector for multi-host');
  grpHostSel.value='h2';
  emit(grpHostSel,'change');
  assert(grpBody.textContent.includes('Book Bot 1')&&!grpBody.textContent.includes('Mini Bot 1'),'M-04: group sheet only lists bots from selected host');
  grpBody.querySelector('input').value='Book Squad';
  await grpBody.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Create Group').action();
  assert(hostCalls.some(c=>c.hid==='h2'&&c.req.action==='bots.groups.create'&&c.req.name==='Book Squad'&&c.req.bot_ids.every(id=>id.startsWith('b2'))),'M-04: bots.groups.create targets selected host with only its bots');

  // M-04 access.redeem: host selector in .bots-redeem-form targets chosen host via rpcForHost.
  const redeemForm=bf.find('.bots-redeem-form');
  const redeemSel=redeemForm.querySelector('select');
  assert(redeemSel,'M-04: access redeem form shows desktop selector for multi-host');
  redeemSel.value='h2';
  emit(redeemSel,'change');
  const redeemInput=redeemForm.querySelector('input');
  redeemInput.value='PAIR-H2';
  await bf.byText('Redeem').action();
  assert(hostCalls.some(c=>c.hid==='h2'&&c.req.action==='bots.access.redeem'&&c.req.code==='PAIR-H2'),'M-04: bots.access.redeem targets selected host');
  bf.abort.abort();
 }

 // M-07 & M-08 (up-next): status:"needs_you", Closed/Snoozed client filtering, legacy daemon fallback, and multi-host items pagination.
 {
  const itemCalls=[];
  const futureMs=Date.now()+3600000;
  const pastMs=Date.now()-60000;
  const h1={id:'h1',name:'Mini',active:true,connection:{phase:'ready'},cached:false,snapshot:{capabilities:{bots:true},sessions:[],orchestration:{}}};
  const h2={id:'h2',name:'Book',active:false,connection:{phase:'ready'},cached:false,snapshot:{capabilities:{bots:true},sessions:[],orchestration:{}}};
  const uf=fixture('up-next',{state:{snapshot:{capabilities:{bots:true}}}});
  uf.ctx.connections=[h1,h2];
  uf.ctx.rpcForHost=async(hid,req)=>{
   itemCalls.push({hid,req});
   if(req.action==='bots.list')return {bots:[{id:'b1',name:'Bot 1'}]};
   if(req.action==='bots.access.list')return {requests:[]};
   if(req.action==='bots.suggestions.list')return {suggestions:[]};
   if(req.action==='bots.items'){
    if(req.status==='needs_you'){
     if(hid==='h1')return req.cursor?{items:[{uid:'n1-p2',owner_bot_id:'b1',title:'Needs H1 P2',status:'needs_you'}],next_cursor:null}:{items:[{uid:'n1',owner_bot_id:'b1',title:'Needs H1',status:'needs_you'}],next_cursor:'ih1'};
     return req.cursor?{items:[{uid:'n2-p2',owner_bot_id:'b1',title:'Needs H2 P2',status:'needs_you'}],next_cursor:null}:{items:[{uid:'n2',owner_bot_id:'b1',title:'Needs H2',status:'needs_you'}],next_cursor:'ih2'};
    }
    if(req.include_closed){
     return {items:[
      {uid:'open-1',owner_bot_id:'b1',title:'Open Leak',status:'working'},
      {uid:'done-1',owner_bot_id:'b1',title:'Done Item',status:'done'},
      {uid:'can-1',owner_bot_id:'b1',title:'Canceled Item',status:'canceled'},
     ],next_cursor:null};
    }
    if(req.include_snoozed){
     return {items:[
      {uid:'unsnoozed',owner_bot_id:'b1',title:'Active Leak',status:'planned',snoozed_until_ms:null},
      {uid:'expired',owner_bot_id:'b1',title:'Expired Snooze',status:'planned',snoozed_until_ms:pastMs},
      {uid:'snoozed-1',owner_bot_id:'b1',title:'Real Snoozed',status:'planned',snoozed_until_ms:futureMs},
     ],next_cursor:null};
    }
    return {items:[],next_cursor:null};
   }
   return {};
  };
  uf.view.update();
  await uf.flush();
  assert(itemCalls.some(c=>c.req.action==='bots.items'&&c.req.status==='needs_you'),'M-07: Needs You sends status:"needs_you"');
  assert(uf.byText('Load more'),'M-08: up-next Load more shown');
  await uf.byText('Load more').action();
  await uf.flush();
  assert(uf.root.textContent.includes('Needs H1 P2')&&uf.byText('Load more'),'M-08: up-next keeps Load more for h2');
  await uf.byText('Load more').action();
  await uf.flush();
  assert(uf.root.textContent.includes('Needs H2 P2')&&!uf.byText('Load more'),'M-08: up-next drains all host cursors');

  // Closed filter renders only done/canceled items.
  await uf.byText('Closed').action();
  await uf.flush();
  assert(uf.root.textContent.includes('Done Item')&&uf.root.textContent.includes('Canceled Item'),'M-07: Closed shows done & canceled');
  assert(!uf.root.textContent.includes('Open Leak'),'M-07: Closed excludes open items');

  // Snoozed filter renders only items with snoozed_until_ms > Date.now().
  await uf.byText('Snoozed').action();
  await uf.flush();
  assert(uf.root.textContent.includes('Real Snoozed'),'M-07: Snoozed shows future-snoozed items');
  assert(!uf.root.textContent.includes('Active Leak')&&!uf.root.textContent.includes('Expired Snooze'),'M-07: Snoozed excludes unsnoozed/expired items');
  uf.abort.abort();

  // Legacy daemon fallback when bots.items rejects unknown field `status` definitively.
  const legacyCalls=[];
  const lf=fixture('up-next',{
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    legacyCalls.push(req);
    if(req.action==='bots.items'){
     if(req.status)throw Object.assign(Error('unknown field `status`'),{definitive:true});
     return {items:[{uid:'ny',owner_bot_id:'b1',title:'Legacy Needs',status:'needs_you'},{uid:'wk',owner_bot_id:'b1',title:'Legacy Working',status:'working'}]};
    }
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'B1'}]};
    return {requests:[],suggestions:[]};
   },
  });
  await lf.flush();
  assert(legacyCalls.some(c=>c.action==='bots.items'&&c.status==='needs_you'));
  assert(legacyCalls.some(c=>c.action==='bots.items'&&!('status' in c)),'M-07: retries without status on definitive rejection');
  assert(lf.root.textContent.includes('Legacy Needs')&&!lf.root.textContent.includes('Legacy Working'),'M-07: client filter still isolates needs_you on legacy daemon');
  lf.abort.abort();
 }

 // M-09: sync-throwing refreshHost/rpc does not crash openCreateBotSheet, mountBotDetail, or details.js accounts panel.
 {
  const sf=fixture('bots',{
   route:{botId:'b1'},
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'Bot 1',agent_id:'claude'}]};
    if(req.action==='bots.models')throw Error('Screen target changed');
    if(req.action==='bots.update')return {bot:{id:'b1',name:'Bot 1',agent_id:'claude'}};
    return {items:[],statuses:[],suggestions:[],requests:[]};
   },
  });
  sf.ctx.refreshHost=()=>{throw Error('Screen target changed');};
  await sf.flush();
  await sf.all().find(n=>n.dataset?.botTab==='settings').action();
  await sf.byText('Agent').action();
  const body=sf.ctx.el('div');
  assert.doesNotThrow(()=>sf.sheets.at(-1)[3](body),'M-09: bots.models sync throw caught in Agent sheet');
  const saveBtn=body.descendants().find(n=>n.tagName==='BUTTON'&&n.getAttribute('aria-label')==='Save');
  await assert.doesNotReject(()=>saveBtn.action(),'M-09: sync-throwing refreshHost after bots.update does not fail save');
  sf.abort.abort();

  const af=fixture('accounts',{rpc:req=>req.action==='switch_account'?{accounts:[{id:'a1',provider:'Claude',email:'a@b.c',active:true,host_default:true}],statuses:[]}:{accounts:[{id:'a1',provider:'Claude',email:'a@b.c',active:false,host_default:true}],statuses:[]}});
  af.ctx.refresh=()=>{throw Error('Screen target changed');};
  await af.flush();
  await assert.doesNotReject(async()=>{await af.find('.account-row').action();await af.flush();},'M-09: details.js accounts panel swallows sync-throwing refresh after switch_account');
  assert.equal(af.errors.length,0,'M-09: accounts switch reports no spurious error on sync-throwing refresh');
  af.abort.abort();
 }

 // M-10 & M-12: single-host fallback (h.id === null) group toggle/active/refresh + focus/draft/select preservation across draw().
 {
  let groupReads=0;
  const f=fixture('bots',{
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',slug:'triage',name:'Triage',agent_id:'claude'}]};
    if(req.action==='bots.items')return {items:[]};
    if(req.action==='bots.access.list')return {requests:[]};
    if(req.action==='bots.connectors.status')return {statuses:[]};
    if(req.action==='bots.groups.list')return {groups:[{id:'grp-1',name:'Core',bot_ids:['b1']}]};
    if(req.action==='bots.groups.read'){groupReads++;return {group:{group:{id:'grp-1',name:'Core',bot_ids:['b1']},threads:[{thread_id:'t1',title:'Main'}],messages:[{sender_name:'Triage',body:'Hi'}],active_run:false}};}
    return {};
   },
  });
  await f.flush();
  await f.all().find(n=>n.dataset?.hubMode==='groups').action();
  // M-10: open group in single-host fallback -> .active class set -> refresh reloads group -> re-click collapses without re-fetching.
  await f.find('.bots-group-row').action();
  await f.flush();
  assert.equal(groupReads,1);
  assert(f.find('.bots-group-row').classList.contains('active'),'M-10: single-host fallback marks open group row .active');
  assert(f.find('.bot-group-panel'),'M-10: group panel opens');

  // M-12: codeInput, modeSelect, and groupTextarea preserve DOM identity, focus, value, and selection across background draw().
  const codeInput=f.find('.bots-redeem-form').querySelector('input');
  codeInput.value='UNSAVED-CODE';
  codeInput.focus();
  const modeSel=f.find('.bot-group-panel').querySelector('select');
  modeSel.value='all';
  emit(modeSel,'change');
  const grpArea=f.find('.bot-group-panel').querySelector('textarea');
  grpArea.value='draft in progress';
  emit(grpArea,'input');
  f.view.update();
  assert.equal(f.find('.bots-redeem-form').querySelector('input'),codeInput,'M-12: codeInput node reused across draw');
  assert.equal(codeInput.value,'UNSAVED-CODE','M-12: codeInput value preserved across draw');
  assert.equal(f.doc.activeElement,codeInput,'M-12: codeInput focus preserved across draw');
  assert.equal(f.find('.bot-group-panel').querySelector('select').value,'all','M-12: group reply mode preserved across draw');
  grpArea.focus();
  f.view.update();
  assert.equal(f.find('.bot-group-panel').querySelector('textarea'),grpArea,'M-12: group textarea reused across draw');
  assert.equal(f.doc.activeElement,grpArea,'M-12: group textarea focus preserved across draw');
  assert.equal(grpArea.value,'draft in progress');

  // M-10: refresh reloads active group on single-host fallback.
  await f.byText('Refresh').action();
  await f.flush();
  assert.equal(groupReads,2,'M-10: refresh reloads active group in single-host fallback');

  // M-10: clicking active group row again collapses it instead of re-fetching.
  await f.find('.bots-group-row').action();
  await f.flush();
  assert.equal(groupReads,2,'M-10: re-clicking active group does not re-fetch');
  assert.equal(f.find('.bot-group-panel'),null,'M-10: re-clicking active group collapses panel');
  f.abort.abort();

  // M-12: up-next item textarea keeps focus across background draw().
  const uf=fixture('up-next',{
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'B1'}]};
    if(req.action==='bots.items')return {items:[{uid:'i1',owner_bot_id:'b1',title:'Check',status:'needs_you'}]};
    return {requests:[],suggestions:[]};
   },
  });
  await uf.flush();
  const commentArea=uf.find('.bot-item-card').querySelector('textarea');
  commentArea.focus();commentArea.value='typing comment';emit(commentArea,'input');
  uf.view.update();
  assert.equal(uf.find('.bot-item-card').querySelector('textarea'),commentArea,'M-12: item textarea reused in place');
  assert.equal(uf.doc.activeElement,commentArea,'M-12: item textarea keeps focus on draw()');
  assert.equal(commentArea.value,'typing comment');
  uf.abort.abort();

  // M-12 & M-15: mountBotDetail keeps External reply + SOUL.md/MEMORY.md focus across draw() AND maps error_category cleanly.
  const df=fixture('bots',{
   route:{botId:'b1'},
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'Bot 1',agent_id:'claude'}]};
    if(req.action==='bots.items')return {items:[]};
    if(req.action==='bots.connectors.status')return {statuses:[
     {bot_id:'b1',source:'telegram',state:'error',error_category:'signed_out'},
     {bot_id:'b1',source:'slack',state:'error',error_category:'usage_limit'},
     {bot_id:'b1',source:'discord',state:'error',error_category:'not_set_up'},
     {bot_id:'b1',source:'gmail',state:'error',error_category:'folder_missing'},
     {bot_id:'b1',source:'linear',state:'error',error_category:'agent_error'},
    ]};
    if(req.action==='bots.soul.get')return {content:'# Initial Soul'};
    if(req.action==='bots.memory.get')return {content:'- Fact 1'};
    return {suggestions:[],requests:[]};
   },
  });
  await df.flush();
  await df.all().find(n=>n.dataset?.botTab==='work').action();
  const replyArea=df.find('.bot-reply-card').querySelector('textarea');
  replyArea.focus();replyArea.value='draft reply';emit(replyArea,'input');
  df.view.update();
  assert.equal(df.find('.bot-reply-card').querySelector('textarea'),replyArea,'M-12: external reply textarea reused');
  assert.equal(df.doc.activeElement,replyArea,'M-12: external reply textarea keeps focus on draw()');
  await df.all().find(n=>n.dataset?.botTab==='settings').action();
  await df.flush();
  for(const label of ['Signed out','Usage limit reached','Not set up','Folder missing','Agent error']){
   assert(df.root.textContent.includes(label),`M-15: connector status renders human label "${label}"`);
  }
  for(const raw of ['signed_out','usage_limit','not_set_up','folder_missing','agent_error']){
   assert(!df.root.textContent.includes(raw),`M-15: raw enum "${raw}" must not appear in UI`);
  }
  const [soulArea,memArea]=df.find('.bot-docs-card').querySelectorAll('textarea');
  soulArea.focus();soulArea.value='# Edited Soul';emit(soulArea,'input');
  df.view.update();
  assert.equal(df.find('.bot-docs-card').querySelectorAll('textarea')[0],soulArea,'M-12: SOUL.md textarea reused');
  assert.equal(df.doc.activeElement,soulArea,'M-12: SOUL.md textarea keeps focus on draw()');
  assert.equal(soulArea.value,'# Edited Soul');
  memArea.focus();memArea.value='- Edited Fact';emit(memArea,'input');
  df.view.update();
  assert.equal(df.find('.bot-docs-card').querySelectorAll('textarea')[1],memArea,'M-12: MEMORY.md textarea reused');
  assert.equal(df.doc.activeElement,memArea,'M-12: MEMORY.md textarea keeps focus on draw()');
  df.abort.abort();
 }

 // M-13: showConnection() keeps #connection-banner visible on main tabs when phase === 'auth_required'.
 {
  const showLine=appSrc.split('\n').find(l=>l.includes('function showConnection('));
  const f=fixture('home'),E=f.root.constructor;
  const banner=new E('div'),notice=new E('div');
  const box=vm.createContext({
   state:{connection:{phase:'idle',message:''},logs:[],cached:false},
   endpoint:{profiles:()=>[{id:'h1',name:'PC',active:true}]},
   banner,notice,activeRouteScreen:'bots',pendingHostAction:null,
   el:(...v)=>new E(...v),t:s=>s,button:(l,a,c)=>new E('button',l,c),
   transport:{reconnect(){}},scanQr(){},hideNotice(){},update(){},Date,Math,
  });
  vm.runInContext(showLine+'\nglobalThis.showConnection=showConnection;',box);
  box.showConnection({phase:'disconnected',message:'Disconnected'});
  assert.equal(banner.hidden,true,'M-13: main tab hides banner on ordinary disconnect');
  box.showConnection({phase:'auth_required',message:'Sign in again'});
  assert.equal(banner.hidden,false,'M-13: main tab shows banner on auth_required');
  f.abort.abort();
 }

 // M-14: app.css defines spacing layout for .bots-items-list and .bot-item-decisions.
 assert(/\.bots-items-list,\.bot-item-decisions\{display:flex;flex-direction:column;gap:10px\}/.test(cssSrc),'M-14: .bots-items-list and .bot-item-decisions gap rules present');

 // M-15: i18n.js provides Korean translations for all 5 connector error categories.
 {
  const iBox=vm.createContext({navigator:{language:'ko'}});
  vm.runInContext(i18nSrc,iBox);
  for(const key of ['Signed out','Usage limit reached','Not set up','Folder missing','Agent error']){
   const ko=iBox.KrillMobile.i18n.dictionary[key];
   assert(typeof ko==='string'&&ko.length>0&&ko!==key,`M-15: Korean translation exists for "${key}"`);
  }
 }
 console.log('PASS mobile audit wave 2 regressions: M-01..M-10, M-12..M-15');
}
async function projectIcons(){
 const store=new Map(),ls={getItem:k=>store.get(k)??null,setItem:(k,v)=>store.set(k,String(v)),removeItem:k=>store.delete(k)};
 const hashA='a'.repeat(16),hashB='b'.repeat(16),hashMiss='c'.repeat(16),pngB64='iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+X2d8AAAAASUVORK5CYII=',svgB64='PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAxNiAxNiIvPg==';
 const hostCalls=[];
 const f=fixture('workspaces',{
  globals:{localStorage:ls},
  rpc:req=>{
   hostCalls.push({host:null,req});
   return {icons:{'k1':{hash:hashA,mime:'image/png',data_base64:pngB64},'k3':{hash:hashB,mime:'image/svg+xml',data_base64:svgB64}},truncated:false};
  },
 });
 // Record requested icon glyph names to verify Lucide mapping and tinted fallback.
 f.ctx.icon=name=>{const svg=f.ctx.el('svg');svg.dataset.iconName=name;return svg;};
 const h1={
  id:'mac-a',name:'Mac A',active:true,connection:{phase:'ready'},cached:false,
  snapshot:{
   capabilities:{repo_icons:true,worktree_create:true},
   repositories:[
    {key:'k1',label:'krill',path:'/krill',icon:{kind:'image',hash:hashA},badge_color:'#737373'},
    {key:'k2',label:'kakaouto',path:'/kakaouto',icon:{kind:'image',hash:hashA}},
    {key:'k3',label:'linear-lite',path:'/linear-lite',icon:{kind:'image',hash:hashB}},
    {key:'k4',label:'drive',path:'/drive',badge_color:'#ef4444'},
    {key:'k5',label:'rocket',path:'/rocket',icon:{kind:'lucide',value:'Rocket'},badge_color:'#22c55e'},
    {key:'k6',label:'missing',path:'/missing',icon:{kind:'image',hash:hashMiss}},
   ],
   workspaces:[
    {key:'w1',repo_key:'k1',label:'main',primary:true},
    {key:'w2',repo_key:'k2',label:'main',primary:true},
    {key:'w3',repo_key:'k3',label:'main',primary:true},
    {key:'w4',repo_key:'k4',label:'main',primary:true},
    {key:'w5',repo_key:'k5',label:'main',primary:true},
    {key:'w6',repo_key:'k6',label:'main',primary:true},
   ],
   sessions:[],
  },
 };
 const h2={
  id:'mac-b',name:'Mac B',active:false,connection:{phase:'ready'},cached:false,
  snapshot:{
   capabilities:{repo_icons:true},
   // Shares hashA with mac-a: deduplicated across hosts so mac-b needs zero RPC!
   repositories:[{key:'kb1',label:'krill-b',path:'/krill-b',icon:{kind:'image',hash:hashA}}],
   workspaces:[{key:'wb1',repo_key:'kb1',label:'main',primary:true}],
   sessions:[],
  },
 };
 f.ctx.connections=[h1,h2];
 f.ctx.rpcForHost=(hostId,req)=>{
  hostCalls.push({host:hostId,req});
  return Promise.resolve({icons:{'k1':{hash:hashA,mime:'image/png',data_base64:pngB64},'k3':{hash:hashB,mime:'image/svg+xml',data_base64:svgB64}},truncated:false});
 };
 // Initial draw paints immediately with zero synchronous RPCs (deferred microtask).
 f.view.update();
 assert.equal(hostCalls.length,0,'icon fetch is deferred until after initial render');
 const markOf=label=>f.all().find(n=>n.classList.contains('repo-toggle')&&n.querySelector('.repo-label').textContent===label).querySelector('.project-icon');
 assert.equal(markOf('drive').querySelector('svg').style.color,'#ef4444','non-neutral badge_color tints folder icon');
 assert.equal(markOf('krill').querySelector('svg').style.color,undefined,'neutral #737373 keeps default muted color');
 assert.equal(markOf('rocket').querySelector('svg').dataset.iconName,'rocket','PC Lucide name maps to mobile SVG path');
 assert.equal(markOf('rocket').querySelector('svg').style.color,'#22c55e','Lucide icon inherits non-neutral badge_color');

 // Deferred batch fetch sends ONE deduplicated RPC for mac-a (k1, k3, k6) and ZERO for mac-b.
 await f.flush();
 assert.equal(hostCalls.length,1,'single deduplicated batch RPC across repos and hosts');
 assert.equal(hostCalls[0].host,'mac-a');
 assert.deepEqual(JSON.parse(JSON.stringify(hostCalls[0].req)),{action:'repo_icons.read',keys:['k1','k3','k6']});
 assert.equal(markOf('krill').querySelector('img').src,`data:image/png;base64,${pngB64}`);
 assert.equal(markOf('kakaouto').querySelector('img').src,`data:image/png;base64,${pngB64}`,'shared hash renders without duplicate fetch');
 assert.equal(markOf('krill-b').querySelector('img').src,`data:image/png;base64,${pngB64}`,'cross-host shared hash renders from single cache');
 assert.equal(markOf('linear-lite').querySelector('img').src,`data:image/svg+xml;base64,${svgB64}`);
 assert.equal(markOf('krill').querySelector('img').decoding,'async');

 // Missing hash is negative-cached: subsequent redraw sends zero retries.
 f.view.update();
 await f.flush();
 assert.equal(hostCalls.length,1,'unreturned hash does not loop on redraw');

 // Broken image decode falls back cleanly to folder glyph.
 const krillImg=markOf('krill').querySelector('img');
 krillImg.onerror();
 assert.equal(markOf('krill').querySelector('img'),null,'onerror removes broken image');
 assert.equal(markOf('krill').querySelector('svg').dataset.iconName,'folder');
 f.abort.abort();

 // Warm relaunch hydrates synchronously from localStorage with ZERO network RPCs.
 const warmCalls=[];
 const w=fixture('workspaces',{globals:{localStorage:ls},rpc:req=>{warmCalls.push(req);return {};}});
 w.state.snapshot={
  capabilities:{repo_icons:true,worktree_create:true},
  repositories:[{key:'k1',label:'krill',path:'/krill',icon:{kind:'image',hash:hashA}}],
  workspaces:[{key:'w1',repo_key:'k1',label:'main',primary:true}],
  sessions:[],
 };
 w.state.groups=new Map([['w1',{key:'w1',repoKey:'k1',label:'main',path:'/krill',primary:true,sessions:[]}]]);
 w.view.update();
 assert.equal(w.all().find(n=>n.classList.contains('repo-toggle')).querySelector('.project-icon').querySelector('img').src,`data:image/png;base64,${pngB64}`,'persisted cache renders synchronously on cold launch');
 await w.flush();
 assert.equal(warmCalls.length,0,'persisted cache requires zero RPCs');
 // Project composer also renders cached image icon in project-choice-dot.
 await w.byText('New workspace').action();
 const sheetBody=w.ctx.el('div');w.sheets.at(-1)[3](sheetBody);
 assert.equal(sheetBody.querySelector('.worktree-project-button').querySelector('.project-choice-dot').querySelector('img').src,`data:image/png;base64,${pngB64}`);
 w.abort.abort();

 // Legacy daemon without capabilities.repo_icons receives zero repo_icons.read RPCs.
 const legacyCalls=[];
 const leg=fixture('workspaces',{rpc:req=>{legacyCalls.push(req);return {};}});
 leg.state.snapshot={
  capabilities:{},
  repositories:[{key:'k1',label:'legacy',path:'/legacy',icon:{kind:'image',hash:'d'.repeat(16)}}],
  workspaces:[{key:'w1',repo_key:'k1',label:'main',primary:true}],
  sessions:[],
 };
 leg.state.groups=new Map([['w1',{key:'w1',repoKey:'k1',label:'main',path:'/legacy',primary:true,sessions:[]}]]);
 leg.view.update();
 await leg.flush();
 assert.equal(legacyCalls.length,0,'legacy daemon is never sent repo_icons.read');
 leg.abort.abort();
 console.log('PASS project icons: deferred batched dedupe RPC, cross-host hash cache, localStorage 0ms warm launch, Lucide/badge tint, onerror fallback, legacy guard');
}
async function todoUxDacRegressions(){
 const botsSrc=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/bots.js'),'utf8');
 assert(!botsSrc.includes('innerHTML'),'bots.js must never use innerHTML');
 assert(!botsSrc.includes('crypto.subtle'),'bots.js must never use crypto.subtle');
 assert(!botsSrc.includes('crypto.randomUUID'),'bots.js must never use crypto.randomUUID');

 // 1. D+A+C on #/up-next: action-type grouping, text-only needs_you comment context + bots.item.answer,
 //    multi-question decision_id, recommended badge, multi-select submit, detail sheet timeline & actions, agy bot compatibility.
 const botsList=[
  {id:'b-agy',slug:'agy-rel',name:'Agy Release',agent_id:'agy',schedules:[{id:'nightly',name:'Nightly ship',cron:'0 9 * * *',enabled:true}]},
  {id:'b-doc',slug:'doc-bot',name:'Doc Bot',agent_id:'claude',schedules:[]},
 ];
 const now=Date.now();
 const itemsList=[
  {
   uid:'it-multi-q',owner_bot_id:'b-agy',title:'Rollout check',status:'needs_you',
   done_means:'All rollout questions answered',
   links:[{dispatch:'disp-7182'},{pr:'https://github.com/craftowen/krill/pull/208'}],
   events:[
    {id:10,kind:'commented',actor:{bot:{bot_id:'b-agy'}},text:'Checks passed. Please pick target and channels.',decisions:[
     {question:'Target env?',options:['Staging','Prod'],recommended:0,multi:false},
     {question:'Notify channels?',options:['Slack','Email','Blog'],recommended:0,multi:true},
    ],created_at_ms:now-120000},
    {id:11,kind:'commented',actor:{owner:{device:'mac'}},text:'Target env? → Staging',decisions:[],created_at_ms:now-60000},
   ],
  },
  {
   uid:'it-text-only',owner_bot_id:'b-agy',title:'Refund wording review',status:'needs_you',
   done_means:'Final refund text approved',
   links:[{dispatch:'disp-9901'}],
   events:[
    {id:20,kind:'commented',actor:{bot:{bot_id:'b-agy'}},text:'Should we add a prorated refund exception line?',decisions:[],created_at_ms:now-30000},
   ],
  },
  {
   uid:'it-stale-dec',owner_bot_id:'b-doc',title:'Follow-up after prior ask',status:'needs_you',
   events:[
    {id:30,kind:'commented',actor:{bot:{bot_id:'b-doc'}},text:'Old ask',decisions:[{question:'Old Q?',options:['OldA','OldB'],recommended:0,multi:false}],created_at_ms:now-90000},
    {id:31,kind:'commented',actor:{owner:{device:'mac'}},text:'OldA',decisions:[],created_at_ms:now-80000},
    {id:32,kind:'status',actor:{bot:{bot_id:'b-doc'}},text:'status -> needs_you',decisions:[],created_at_ms:now-20000},
    {id:33,kind:'commented',actor:{bot:{bot_id:'b-doc'}},text:'Need fresh feedback on paragraph 2.',decisions:[],created_at_ms:now-10000},
   ],
  },
 ];
 const uf=fixture('up-next',{
  state:{snapshot:{capabilities:{bots:true},sessions:[{id:41,state:'Blocked',title:'Build token expired',worktree_key:'w-build'}],orchestration:{gates:[{id:'g-db',status:'pending',question:'Run index cleanup?',options:['Approve','Deny']}],questions:[],runs:[],tasks:[]}}},
  rpc:req=>{
   if(req.action==='bots.list')return {bots:botsList};
   if(req.action==='bots.items')return {items:itemsList};
   if(req.action==='bots.access.list')return {requests:[{token:'acc-1',bot_id:'b-doc',source:'slack',sender_name:'Designer',status:'pending'}]};
   if(req.action==='bots.suggestions.list')return {suggestions:[{id:'sug-9',bot_id:'b-doc',title:'Summarize notes',why:'3 docs',ask:'Start now?'}]};
   if(req.action==='bots.upcoming')return {schedules:[{id:'nightly',running:false,last_error:'Token expired'}]};
   return {ok:true};
  },
 });
 uf.ctx.closeSheet=()=>{};
 await uf.flush();

 // D: All 4 action groups render with clear headings.
 const groups=uf.all().filter(n=>n.dataset?.todoGroup).map(n=>n.dataset.todoGroup);
 assert.deepEqual(groups,['choices','approvals','blocked'],'D: action-type groups render in priority order');
 assert(uf.root.textContent.includes('Choices needed'),'D: Choices needed heading rendered');
 assert(uf.root.textContent.includes('Approvals needed'),'D: Approvals needed heading rendered');
 assert(uf.root.textContent.includes('Blocked & errors'),'D: Blocked & errors heading rendered');
 assert(uf.root.textContent.includes('Failed automation')&&uf.root.textContent.includes('Nightly ship'),'D: failed automation shown in Blocked & errors');
 assert(uf.root.textContent.includes('Waiting sessions')&&uf.root.textContent.includes('Build token expired'),'D: waiting session shown in Blocked & errors');

 // A: Text-only needs_you card shows bot's latest comment, hides raw dispatch: disp-9901, and submits via bots.item.answer!
 const textCard=uf.all().find(n=>n.dataset?.item==='it-text-only');
 assert(textCard,'text-only needs_you card rendered');
 assert(textCard.textContent.includes('Should we add a prorated refund exception line?'),'A: latest bot comment rendered on text-only needs_you card');
 assert(!textCard.textContent.includes('disp-9901'),'A: raw dispatch ID hidden on compact card');
 const textArea=textCard.querySelector('textarea');
 assert(textArea,'A: inline reply box shown for text-only needs_you item');
 textArea.value='Add one line for prorated refunds.';
 emit(textArea,'input');
 await textCard.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Send answer').action();
 assert(uf.calls.some(c=>c.action==='bots.item.answer'&&c.item_id==='it-text-only'&&c.answer_text==='Add one line for prorated refunds.'),'root-cause fix: text-only needs_you calls bots.item.answer instead of bots.item.update');

 // Stale decisions after owner reply + new text-only needs_you must NOT reappear.
 const staleCard=uf.all().find(n=>n.dataset?.item==='it-stale-dec');
 assert(staleCard&&!staleCard.textContent.includes('OldA'),'waitingQuestion hides stale answered decisions');
 assert(staleCard.textContent.includes('Need fresh feedback on paragraph 2.'),'shows fresh bot context comment');

 // Multi-question + answered line + Recommended badge + Multi-select toggle & submit with decision_id!
 const multiCard=uf.all().find(n=>n.dataset?.item==='it-multi-q');
 assert(multiCard.textContent.includes('Target env? → Staging'),'answered question collapses to summary line');
 assert(multiCard.textContent.includes('Recommended'),'Recommended badge rendered on recommended option');
 assert(multiCard.textContent.includes('Multi-select'),'Multi-select badge rendered on multi decision');
 assert(!multiCard.textContent.includes('disp-7182'),'raw dispatch ID hidden on compact card');
 assert(multiCard.textContent.includes('PR #208'),'human-readable PR link rendered');
 const slackBtn=multiCard.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Slack');
 const blogBtn=multiCard.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Blog');
 await slackBtn.action();
 await blogBtn.action();
 const submitMulti=multiCard.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Submit (2)');
 assert(submitMulti&&!submitMulti.disabled,'Submit (2) enabled after picking two options');
 await submitMulti.action();
 assert(uf.calls.some(c=>c.action==='bots.item.answer'&&c.item_id==='it-multi-q'&&c.decision_id==='10:1'&&c.answer_text==='Notify channels? → Slack, Blog'),'multi-select submits joined named answer with exact decision_id');

 // C: Details button opens C-sheet with full Activity timeline, Linked task, Open chat, Snooze 4h, Done, Add link, Cancel item.
 const detailBtn=multiCard.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Details');
 await detailBtn.action();
 const sheetSpec=uf.sheets.at(-1);
 assert.equal(sheetSpec[0],'Rollout check','C: detail sheet opens with item title');
 const sheetBody=uf.ctx.el('div');
 sheetSpec[3](sheetBody);
 assert(sheetBody.textContent.includes('Activity')&&sheetBody.textContent.includes('Checks passed. Please pick target and channels.')&&sheetBody.textContent.includes('Target env? → Staging'),'C: full event timeline shown in detail sheet');
 assert(sheetBody.textContent.includes('Linked task')&&!sheetBody.textContent.includes('disp-7182'),'C: dispatch link rendered as human-readable Linked task');
 await sheetBody.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Snooze 4h').action();
 assert(uf.calls.some(c=>c.action==='bots.item.update'&&c.item_id==='it-multi-q'&&typeof c.snoozed_until_ms?.set==='number'),'C: Snooze 4h works from detail sheet');
 await sheetBody.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Done').action();
 assert(uf.calls.some(c=>c.action==='bots.item.update'&&c.item_id==='it-multi-q'&&c.status==='done'),'C: Done works from detail sheet');
 await sheetBody.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Add link').action();
 const linkInp=sheetBody.querySelector('input');
 linkInp.value='https://krill.page/docs';
 await sheetBody.querySelectorAll('button').filter(b=>b.getAttribute('aria-label')==='Add link').at(-1).action();
 assert(uf.calls.some(c=>c.action==='bots.item.link'&&c.item_id==='it-multi-q'&&c.link?.url==='https://krill.page/docs'),'C: Add link works from detail sheet');
 await sheetBody.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Open chat').action();
 assert.equal(uf.routes.at(-1),'/bots/b-agy','C: Open chat navigates to agy bot workspace');
 uf.abort.abort();

 // 2. Korean i18n coverage for all new To Do D+A+C copy.
 const koBox=vm.createContext({navigator:{language:'ko'}});
 vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/i18n.js'),'utf8'),koBox);
 const K=koBox.KrillMobile.i18n;
 for(const key of ['Choices needed','Approvals needed','Blocked & errors','Recommended','Send answer','Type your answer…','Select at least one option','Activity','No activity yet.','Auto','Linked task','Open chat','Item details','Add a comment…','Latest update','Multi-select','Answered','File']){
  assert.notEqual(K.t(key),key,`missing ko translation for "${key}"`);
 }
 assert.equal(K.t('Submit ({0})',2),'2개 선택 보내기');
 assert.equal(K.t('Details ({0})',3),'상세 (3)');
 console.log('PASS To Do D+A+C regressions: action groups, bot context, bots.item.answer, decision_id, multi-select, detail sheet, agy bot, ko i18n, security');
}
async function voicePipitoRegressions(){
 // 1. WAV encoding, chunked upload & single-frame upload, insertText, and pre-send edit voice.feedback.
 const f=fixture();
 const appSrc=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/app.js'),'utf8');
 assert(appSrc.includes('M.voice='),'app.js exports M.voice');
 assert(appSrc.includes('voice.warm')&&appSrc.includes('voice.transcribe')&&appSrc.includes('voice.feedback'),'app.js wires voice RPCs');
 // Extract pure helpers via lightweight VM sandbox
 const vBox=vm.createContext({
  btoa:s=>Buffer.from(s,'binary').toString('base64'),
  atob:s=>Buffer.from(s,'base64').toString('binary'),
  Float32Array,Uint8Array,ArrayBuffer,DataView,Event:class Event{constructor(t){this.type=t;}},
  Date,Math,Promise,Error,setInterval,clearInterval,
  isSecureContext:true,
  navigator:{mediaDevices:{getUserMedia:async()=>({getTracks:()=>[{stop(){}}]})}},
  AudioContext:class{
   constructor(){this.sampleRate=48000;this.destination={};}
   createMediaStreamSource(){return {connect(){},disconnect(){}};}
   createScriptProcessor(){const p={connect(){},disconnect(){},onaudioprocess:null};setTimeout(()=>p.onaudioprocess?.({inputBuffer:{getChannelData:()=>new Float32Array(480)}}),0);return p;}
   close(){return Promise.resolve();}
  },
 });
 // Evaluate helper snippet from app.js
 const startIdx=appSrc.indexOf('const voiceEncodeWav=');
 const endIdx=appSrc.indexOf('const editable=node=>');
 assert(startIdx>0&&endIdx>startIdx,'app.js voice helper block found');
 const snippet=`(()=>{const M=globalThis.KrillMobile={};const el=(t,tx,c)=>({tag:t,textContent:tx||'',className:c||'',children:[],hidden:false,append(...a){this.children.push(...a);},insertBefore(n){this.children.unshift(n);}});const button=(l,fn,c)=>{const b=el('button',l,c);b.action=fn;b.setAttribute=()=>{};return b;};const t=s=>s;\n${appSrc.slice(startIdx,endIdx)}\n})();`;
 vm.runInContext(snippet,vBox);
 const V=vBox.KrillMobile.voice;
 // WAV header verification (48kHz -> 16kHz downsampled)
 const wav=V.encodeWav([new Float32Array(4800)],48000);
 assert.equal(Buffer.from(wav.slice(0,4)).toString(),'RIFF');
 assert.equal(Buffer.from(wav.slice(8,12)).toString(),'WAVE');
 assert.equal(new DataView(wav.buffer).getUint32(24,true),16000);
 assert.equal(wav.length,44+1600*2);
 // Single frame vs chunked upload
 const rpcCalls=[];
 const mockRpc=async req=>{
  rpcCalls.push(req);
  if(req.action==='voice.transcribe'){
   if(req.total_chunks>1&&req.chunk_index+1<req.total_chunks)return {chunk_accepted:true};
   return {silence:false,text:'크릴 데몬 확인'};
  }
  return {ok:true};
 };
 const res1=await V.transcribeBytes(mockRpc,new Uint8Array(1000),{targetKind:'session',targetId:'7'});
 assert.equal(res1.text,'크릴 데몬 확인');
 assert.equal(rpcCalls.at(-1).target_kind,'session');
 assert(typeof rpcCalls.at(-1).audio_wav_b64==='string');
 rpcCalls.length=0;
 const res2=await V.transcribeBytes(mockRpc,new Uint8Array(600*1024),{targetKind:'group_chat',targetId:'g1'});
 assert.equal(res2.text,'크릴 데몬 확인');
 assert.equal(rpcCalls.length,3,'600 KiB splits into 3 chunks of 256/256/88 KiB');
 assert.equal(rpcCalls[0].chunk_index,0);
 assert.equal(rpcCalls[2].chunk_index,2);
 assert.equal(rpcCalls[2].total_chunks,3);
 // Attach to composer, record -> stop -> insert -> edit -> onSend fires voice.feedback
 const hostNode={children:[],append(...a){this.children.push(...a);},insertBefore(n){this.children.unshift(n);}};
 const ta={value:'',selectionStart:0,selectionEnd:0,events:[],setSelectionRange(a,b){this.selectionStart=a;this.selectionEnd=b;},dispatchEvent(e){this.events.push(e.type);},focus(){}};
 rpcCalls.length=0;
 const ctl=V.attach(ta,hostNode,{ctx:{rpc:mockRpc},targetKind:'bot_chat',targetId:()=>'b1'});
 const micBtn=ctl.el.children[0];
 await micBtn.action();
 await new Promise(r=>setTimeout(r,15));
 await ctl.stop(false);
 assert.equal(ta.value,'크릴 데몬 확인');
 assert(ta.events.includes('input'));
 assert(rpcCalls.some(c=>c.action==='voice.warm'&&c.target_kind==='bot_chat'&&c.target_id==='b1'));
 // User edits before send -> voice.feedback fires
 ctl.onSend('krilld 확인');
 await new Promise(r=>setTimeout(r,10));
 assert(rpcCalls.some(c=>c.action==='voice.feedback'&&c.before==='크릴 데몬 확인'&&c.after==='krilld 확인'));
 // 2. Mobile Settings Voice / Pipito section
 const voiceSettingsCalls=[];
 const sf=fixture('settings',{
  rpc:async req=>{
   voiceSettingsCalls.push(req);
   if(req.action==='voice.status')return {available:true,status:'ready',refine_enabled:true,vocab_count:3};
   if(req.action==='voice.config_get')return {refine_enabled:true,desktop_shortcut:'Alt+Shift+Space',pinned_vocab:['Pipito'],ignored_vocab:[],auto_vocab:[{word:'krilld',count:4,last_seen_ms:1}]};
   if(req.action==='voice.config_set')return {refine_enabled:req.refine_enabled??true,desktop_shortcut:'Alt+Shift+Space',pinned_vocab:req.add_pinned?['Pipito',req.add_pinned]:['Pipito'],ignored_vocab:[],auto_vocab:[]};
   return {};
  },
 });
 sf.ctx.connections=[{id:'h1',name:'Mac',active:true,connection:{phase:'ready'},snapshot:{sessions:[]}}];
 sf.view.update();
 await sf.flush();
 await new Promise(r=>setTimeout(r,20));
 assert(voiceSettingsCalls.some(c=>c.action==='voice.status'));
 assert(voiceSettingsCalls.some(c=>c.action==='voice.config_get'));
 assert(sf.all().some(n=>String(n.textContent||'').includes('Voice / Pipito')));
 assert(sf.all().some(n=>String(n.textContent||'').includes('Pipito')));
 assert(sf.all().some(n=>String(n.textContent||'').includes('krilld (4)')));
 sf.abort.abort();
 // 3. Korean i18n strings for Voice / Pipito
 const koBox=vm.createContext({navigator:{language:'ko'}});
 vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/i18n.js'),'utf8'),koBox);
 const K=koBox.KrillMobile.i18n;
 for(const key of ['Voice input','Stop recording','Transcribing…','Voice / Pipito','AI Refine','Pipito not running','Pipito not installed']){
  assert.notEqual(K.t(key),key,`missing ko translation for "${key}"`);
 }
 assert.equal(K.t('Learned words ({0})',5),'자동 학습 단어장 (5)');
 console.log('PASS Voice / Pipito mobile regressions: WAV encoding, chunked upload, composer insert & feedback, settings UI, ko i18n');
}
async function sweepW2MobileRegressions(){
 const botsSrc=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/bots.js'),'utf8');
 const i18nSrc=fs.readFileSync(path.join(__dirname,'../src/mobile/assets/i18n.js'),'utf8');

 // MB-02 + MB-53 + MB-55: Multi-decision detail sheet stays open and refreshes after Q1, closes after Q2; Done clears draft; Add link uses type=text.
 {
  let closeCount=0;
  let itemState={
   uid:'it-mb02',owner_bot_id:'b1',title:'Two decisions',status:'needs_you',
   events:[{id:100,kind:'commented',actor:{bot:{bot_id:'b1'}},text:'Pick two',decisions:[
    {question:'First?',options:['A1','A2'],recommended:0,multi:false},
    {question:'Second?',options:['B1','B2'],recommended:0,multi:false},
   ],created_at_ms:Date.now()-5000}],
  };
  const uf=fixture('up-next',{
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'Bot 1'}]};
    if(req.action==='bots.items')return {items:[itemState]};
    if(req.action==='bots.item.answer'){
     const nextEvents=[...itemState.events,{id:itemState.events.length+101,kind:'commented',actor:{owner:{device:'mobile'}},text:req.answer_text,decisions:[],created_at_ms:Date.now()}];
     const done=nextEvents.length>=3;
     itemState={...itemState,status:done?'working':'needs_you',events:nextEvents};
     return {item:itemState};
    }
    return {requests:[],suggestions:[]};
   },
  });
  uf.ctx.closeSheet=()=>{closeCount++;};
  await uf.flush();
  const card=uf.all().find(n=>n.dataset?.item==='it-mb02');
  await card.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Details').action();
  const body=uf.ctx.el('div');
  uf.sheets.at(-1)[3](body);
  // MB-55: Add link input in detail sheet is type=text (allows file paths).
  const sheetInputs=body.querySelectorAll('input');
  assert(sheetInputs.some(inp=>inp.type==='text'&&inp.getAttribute('aria-label')==='Link URL'),'MB-55: detail sheet link input uses type=text');
  // MB-02: Answer Q1 -> sheet stays open and refreshes Q1 to answered while keeping Q2 open.
  await body.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='A1').action();
  assert.equal(closeCount,0,'MB-02: detail sheet stays open when open decisions remain');
  assert(body.textContent.includes('First? → A1'),'MB-02: answered Q1 reflected in refreshed detail sheet');
  const b2Btn=body.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='B2');
  assert(b2Btn,'MB-02: Q2 buttons remain rendered in detail sheet');
  await b2Btn.action();
  assert.equal(closeCount,1,'MB-02: detail sheet closes after final decision answered');
  // MB-53: Done with closing comment clears input and draftKey.
  const ta=body.querySelector('textarea');
  ta.value='Closing note';emit(ta,'input');
  assert.equal(uf.state.drafts.get('bot-item:it-mb02'),'Closing note');
  await body.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Done').action();
  assert.equal(ta.value,'','MB-53: Done clears detail sheet textarea');
  assert.equal(uf.state.drafts.has('bot-item:it-mb02'),false,'MB-53: Done removes draftKey from state.drafts');
  uf.abort.abort();
 }

 // MB-24: To Do Closed and Snoozed tabs pass status="closed" and status="snoozed".
 {
  const itemCalls=[];
  const uf=fixture('up-next',{
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.items'){itemCalls.push(req);return {items:[]};}
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'B1'}]};
    return {requests:[],suggestions:[]};
   },
  });
  await uf.flush();
  assert.equal(itemCalls.at(-1).status,'needs_you');
  await uf.all().find(n=>n.dataset?.filter==='closed').action();
  await uf.flush();
  assert.equal(itemCalls.at(-1).status,'closed','MB-24: Closed tab sends status="closed"');
  await uf.all().find(n=>n.dataset?.filter==='snoozed').action();
  await uf.flush();
  assert.equal(itemCalls.at(-1).status,'snoozed','MB-24: Snoozed tab sends status="snoozed"');
  uf.abort.abort();
 }

 // MB-25: mountBotDetail mounted while offline triggers load() on reconnect.
 {
  let listCalls=0;
  const df=fixture('bots',{
   route:{botId:'b1'},
   state:{connection:{phase:'connecting',message:'Connecting…'},snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list'){listCalls++;return {bots:[{id:'b1',name:'Reconnected Bot',agent_id:'claude'}]};}
    return {items:[],statuses:[],suggestions:[],requests:[],sessions:[],groups:[]};
   },
  });
  await df.flush();
  assert.equal(listCalls,0,'MB-25: offline mount skips load');
  df.state.connection={phase:'ready',message:'Connected'};
  df.view.update();
  await df.flush();
  assert.equal(listCalls,1,'MB-25: reconnect update triggers load()');
  assert(df.root.textContent.includes('Reconnected Bot'),'MB-25: bot details render after reconnect');
  df.abort.abort();
 }

 // MB-26: Decision Deck handles multi:true and empty options[] via Answer question button opening detail sheet.
 {
  const deckItems=[
   {uid:'it-multi',owner_bot_id:'b1',title:'Multi pick',status:'needs_you',events:[{id:1,kind:'commented',actor:{bot:{bot_id:'b1'}},text:'Choose many',decisions:[{question:'Channels?',options:['Slack','Email'],multi:true}],created_at_ms:100}]},
   {uid:'it-empty',owner_bot_id:'b1',title:'Free form',status:'needs_you',events:[{id:2,kind:'commented',actor:{bot:{bot_id:'b1'}},text:'Tell me',decisions:[{question:'Why?',options:[],multi:false}],created_at_ms:200}]},
  ];
  const bf=fixture('bots',{
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'B1'}]};
    if(req.action==='bots.items')return {items:deckItems};
    return {requests:[],statuses:[],groups:[],sessions:[]};
   },
  });
  await bf.flush();
  const deck1=bf.find('.bot-decision-deck');
  assert(deck1.descendants().some(b=>b.tagName==='BUTTON'&&b.getAttribute('aria-label')==='Answer question'),'MB-26: multi-select renders Answer question button instead of 1-tap submit');
  assert(!deck1.descendants().some(b=>b.tagName==='BUTTON'&&b.getAttribute('aria-label')==='Slack'),'MB-26: multi-select does not render 1-tap option button');
  await deck1.descendants().find(b=>b.tagName==='BUTTON'&&b.getAttribute('aria-label')==='Answer question').action();
  assert.equal(bf.sheets.at(-1)[0],'Multi pick','MB-26: Answer question opens item detail sheet');
  await deck1.descendants().find(b=>b.tagName==='BUTTON'&&b.getAttribute('aria-label')==='Next').action();
  const deck2=bf.find('.bot-decision-deck');
  assert(deck2.descendants().some(b=>b.tagName==='BUTTON'&&b.getAttribute('aria-label')==='Answer question'),'MB-26: empty options[] renders Answer question button');
  bf.abort.abort();
 }

 // MB-27 (bots.js) + MB-52: Bot detail preserves synthetic session across snapshot wipe & excludes snoozed needs_you from decision deck & badge.
 {
  const mountedOpts=[];
  const df=fixture('bots',{
   route:{botId:'b1'},
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'Bot 1',agent_id:'claude'}]};
    if(req.action==='bots.items'){
     if(req.status==='needs_you')return {items:[]};
     return {items:[
      {uid:'active-ny',owner_bot_id:'b1',title:'Active Ask',status:'needs_you'},
      {uid:'snoozed-ny',owner_bot_id:'b1',title:'Snoozed Ask',status:'needs_you',snoozed_until_ms:Date.now()+3600000},
     ]};
    }
    if(req.action==='bots.sessions.list')return {sessions:[{session_id:77,bot_id:'b1',live:true,working:false,label:'Home'}]};
    return {statuses:[],suggestions:[],requests:[],groups:[]};
   },
  });
  df.M.chat={mount(_el,opts){mountedOpts.push(opts);return {setVisible(){},update(){},unmount(){}};}};
  await df.flush();
  assert.equal(mountedOpts[0]?.botSession,true,'MB-27: bots.js passes botSession:true to M.chat.mount');
  assert(df.state.sessions.has(77),'MB-27: synthetic bot session inserted on mount');
  // Simulate background snapshot wiping state.sessions:
  df.state.sessions=new Map();
  df.view.update();
  assert(df.state.sessions.has(77),'MB-27: synthetic bot session re-ensured on update() after snapshot wipe');
  // MB-52: Work badge counts only unsnoozed needs_you (1, not 2) and deck excludes Snoozed Ask.
  const workTab=df.all().find(n=>n.dataset?.botTab==='work');
  assert(workTab.textContent.includes('1')&&!workTab.textContent.includes('2'),'MB-52: Work tab badge excludes actively snoozed needs_you items');
  assert(!df.find('.bot-decision-deck').textContent.includes('Snoozed Ask'),'MB-52: Bot decision deck excludes actively snoozed needs_you items');
  df.abort.abort();
 }

 // MB-29: Accounts panel setHost() while Host A RPC is in flight fetches Host B and ignores Host A late response.
 {
  const pending={};
  const af=fixture('accounts',{
   rpc:req=>new Promise((resolve,reject)=>{pending[req._host||'h1']={resolve,reject};}),
  });
  let hostConns=[
   {id:'h1',name:'Host A',active:true,connection:{phase:'ready'}},
   {id:'h2',name:'Host B',active:false,connection:{phase:'ready'}},
  ];
  const rpcHosts=[];
  const panelRoot=af.ctx.el('div');
  const panelCtx={
   ...af.ctx,
   getConnections:()=>hostConns,
   rpcForHost:(hid,req)=>{rpcHosts.push(hid);return new Promise((resolve,reject)=>{pending[hid]={resolve,reject};});},
  };
  const panel=af.M.mountAccountsPanel(panelRoot,panelCtx,{hostId:'h1'});
  assert.deepEqual(rpcHosts,['h1']);
  panel.setHost('h2');
  assert.deepEqual(rpcHosts,['h1','h2'],'MB-29: setHost immediately starts load for Host B despite Host A in flight');
  pending.h1.resolve({accounts:[{id:'a-old',provider:'Claude',email:'hostA@example.com',active:true,host_default:true}],statuses:[]});
  await af.flush();
  assert(!panelRoot.textContent.includes('hostA@example.com'),'MB-29: late Host A response ignored after setHost(h2)');
  pending.h2.resolve({accounts:[{id:'a-new',provider:'Claude',email:'hostB@example.com',active:true,host_default:true}],statuses:[]});
  await af.flush();
  assert(panelRoot.textContent.includes('hostB@example.com'),'MB-29: Host B accounts render cleanly');
  af.abort.abort();
 }

 // MB-30: workspaceComposer Run On switcher calls openWorkspaceComposer (skipping chooser/filter override).
 {
  let openedRepo=null,selectedTarget=null,newWsCalls=0;
  const wf=fixture('workspaces',{
   state:{snapshot:{capabilities:{worktree_create:true},repositories:[{key:'r1',label:'Repo1',path:'/r1'}],agents:[{id:'claude',name:'Claude'}]}},
  });
  const conns=[
   {id:'h1',name:'Host A',active:true,connection:{phase:'ready'},snapshot:{capabilities:{worktree_create:true},repositories:[{key:'r1'}]}},
   {id:'h2',name:'Host B',active:false,connection:{phase:'ready'},snapshot:{capabilities:{worktree_create:true},repositories:[{key:'r1'}]}},
  ];
  const compCtx={
   ...wf.ctx,
   getConnections:()=>conns,
   getHosts:()=>conns,
   selectHost:(id,cb)=>{selectedTarget=id;cb?.();},
   newWorkspace:()=>{newWsCalls++;},
   openWorkspaceComposer:repo=>{openedRepo=repo;},
   closeSheet:()=>{},
  };
  wf.M.workspaceComposer(compCtx,'r1');
  const sheetBody=wf.ctx.el('div');
  wf.sheets.at(-1)[3](sheetBody);
  const hostBBtn=sheetBody.descendants().find(n=>n.tagName==='BUTTON'&&n.getAttribute('aria-label')==='Host B');
  await hostBBtn.action();
  assert.equal(selectedTarget,'h2','MB-30: switched to Host B');
  assert.equal(openedRepo,'r1','MB-30: invoked openWorkspaceComposer directly');
  assert.equal(newWsCalls,0,'MB-30: did not bounce back through newWorkspace deviceFilter/chooser');
  wf.abort.abort();
 }

 // MB-48 + MB-55: renderItemsList updates sync/onReload across re-renders; Add link sheet input uses type=text.
 {
  let reloadCount=0;
  const uf=fixture('up-next',{
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'Initial Name'}]};
    if(req.action==='bots.items')return {items:[{uid:'i-48',owner_bot_id:'b1',title:'Task',status:'needs_you'}]};
    return {requests:[],suggestions:[]};
   },
  });
  await uf.flush();
  const card=uf.all().find(n=>n.dataset?.item==='i-48');
  await card.querySelectorAll('button').find(b=>b.getAttribute('aria-label')==='Add link').action();
  const linkSheetBody=uf.ctx.el('div');
  uf.sheets.at(-1)[3](linkSheetBody);
  assert.equal(linkSheetBody.querySelector('input').type,'text','MB-55: renderItemsList Add link input uses type=text');
  assert(botsSrc.includes('entry.sync = syncItem;')&&botsSrc.includes('entry.onReload = onReload;'),'MB-48: renderItemsList refreshes entry.sync and entry.onReload on each render');
  uf.abort.abort();
  assert.equal(reloadCount,0);
 }

 // MB-49 + MB-50: Bots Hub sends status="needs_you" & multi-host partial failure still shows healthy host data.
 {
  const hostItemReqs=[];
  const bf=fixture('bots',{
   state:{snapshot:{capabilities:{bots:true}}},
  });
  bf.ctx.connections=[
   {id:'h1',name:'Healthy PC',active:true,connection:{phase:'ready'},snapshot:{capabilities:{bots:true}}},
   {id:'h2',name:'Broken PC',active:false,connection:{phase:'ready'},snapshot:{capabilities:{bots:true}}},
  ];
  bf.ctx.rpcForHost=(hid,req)=>{
   if(req.action==='bots.items')hostItemReqs.push({hid,req});
   if(hid==='h2')return Promise.reject(Error('RPC timeout'));
   if(req.action==='bots.list')return Promise.resolve({bots:[{id:'b1',name:'Healthy Bot',agent_id:'claude'}]});
   if(req.action==='bots.items')return Promise.resolve({items:[]});
   return Promise.resolve({requests:[],statuses:[],groups:[],sessions:[]});
  };
  bf.view.update();
  await bf.flush();
  assert(hostItemReqs.some(x=>x.hid==='h1'&&x.req.status==='needs_you'),'MB-49: Bots Hub requests bots.items with status="needs_you"');
  assert(bf.root.textContent.includes('Healthy Bot'),'MB-50: Bots Hub preserves healthy host bots when peer host fails');
  assert(bf.root.textContent.includes('Broken PC'),'MB-50: Bots Hub surfaces failing host error');
  bf.abort.abort();
 }

 // MB-51: up-next failed automation Run now catches BotSessionRefusal and shows localized toast.
 {
  const toasts=[];
  const uf=fixture('up-next',{
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'B1',schedules:[{id:'s1',name:'Nightly'}]}]};
    if(req.action==='bots.upcoming')return {schedules:[{id:'s1',last_error:'failed'}]};
    if(req.action==='bots.schedule.run')throw Object.assign(Error('raw desktop on this computer error'),{data:{refusal:{code:'bot_paused',detail:'B1'}}});
    return {items:[],requests:[],suggestions:[]};
   },
  });
  uf.ctx.toast=msg=>toasts.push(msg);
  await uf.flush();
  const runBtn=uf.all().find(b=>b.tagName==='BUTTON'&&b.getAttribute('aria-label')==='Run now');
  await assert.doesNotReject(()=>runBtn.action(),'MB-51: Run now catches refusal error');
  assert(toasts.some(m=>m.includes("Bot 'B1' is paused")),'MB-51: localized mobileBotRefusalText shown in toast');
  uf.abort.abort();
 }

 // MB-54: Legacy single-host without capabilities.bots does not fire bots.* RPCs on #/bots or #/up-next.
 {
  const bf=fixture('bots',{state:{snapshot:{capabilities:{bots:false}}}});
  await bf.flush();
  assert.equal(bf.calls.filter(c=>String(c.action).startsWith('bots.')).length,0,'MB-54: #/bots skips bots.* RPCs on legacy single host');
  bf.abort.abort();
  const uf=fixture('up-next',{state:{snapshot:{capabilities:{bots:false}}}});
  await uf.flush();
  assert.equal(uf.calls.filter(c=>String(c.action).startsWith('bots.')).length,0,'MB-54: #/up-next skips bots.* RPCs on legacy single host');
  uf.abort.abort();
 }

 // MB-56: i18n.js has zero duplicate keys and consistent translations.
 {
  const lines=i18nSrc.split('\n');
  const seen=new Map();
  lines.forEach((line,idx)=>{
   if(idx+1<6||idx+1>1125)return;
   const m=line.match(/^\s*(?:"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'|([A-Za-z_$][A-Za-z0-9_$]*))\s*:/);
   if(!m)return;
   const key=m[1]!==undefined?JSON.parse('"'+m[1]+'"'):m[2]!==undefined?m[2]:m[3];
   assert(!seen.has(key),`MB-56: duplicate i18n key "${key}" at L${idx+1} (first L${seen.get(key)})`);
   seen.set(key,idx+1);
  });
  const koBox=vm.createContext({navigator:{language:'ko'}});
  vm.runInContext(i18nSrc,koBox);
  assert.equal(koBox.KrillMobile.i18n.t('Desktop'),'컴퓨터','MB-56: Desktop translates consistently to 컴퓨터');
  assert.equal(koBox.KrillMobile.i18n.t('No matches'),'검색 결과가 없습니다','MB-56: No matches standardized');
  assert.equal(koBox.KrillMobile.i18n.t('Clear'),'지우기','MB-56: Clear standardized');
 }

 // MB-63: commandEditor removes rejected command_catalog promise from cache so switching back retries.
 {
  let catCalls=0;
  const sessions=new Map([
   [1,{id:1,agent:'claude',state:'Working',groupKey:'repo',cwd:'/repo',command_context:'ctx1'}],
   [2,{id:2,agent:'claude',state:'Working',groupKey:'repo',cwd:'/repo2',command_context:'ctx2'}],
  ]);
  const groups=new Map([['repo',{label:'Project',sessions:[...sessions.values()]}]]);
  const sf=fixture('settings',{
   route:{subpage:'terminal'},
   state:{sessions,groups,snapshot:{capabilities:{command_catalog:true},quick_commands:[]}},
   rpc:req=>{
    if(req.action==='command_catalog'){
     catCalls++;
     if(catCalls===1)return Promise.reject(Error('timeout'));
     return Promise.resolve({supported:true,items:[{name:'Deploy',text:'/deploy',description:'ship'}]});
    }
    return {};
   },
  });
  await sf.flush();
  await sf.byText('Find installed skills').action();
  await sf.flush();
  const sel=sf.all().find(n=>n.getAttribute('aria-label')==='Session for installed skills');
  sel.value='1';emit(sel,'change');await sf.flush();
  assert.equal(catCalls,1);
  // Switch session away and back -> must retry instead of re-awaiting rejected cache entry
  sel.value='2';emit(sel,'change');await sf.flush();
  sel.value='1';emit(sel,'change');await sf.flush();
  assert.equal(catCalls,3,'MB-63: switching back retries command_catalog after earlier rejection');
  assert(sf.root.textContent.includes('Deploy'),'MB-63: retried catalog renders installed skills');
  sf.abort.abort();
 }

 // MB-65: Krill Drop download() aborts before saving when ctx.signal aborts before final drop_get resolves.
 {
  let resolveChunk,shared=0;
  const df=fixture('drop',{
   globals:{
    atob:s=>Buffer.from(s,'base64').toString('binary'),
    Blob:class{constructor(parts){this.parts=parts;}},
    File:class{constructor(parts,name){this.parts=parts;this.name=name;}},
    navigator:{canShare:()=>true,share:async()=>{shared++;}},
   },
   rpc:req=>{
    if(req.action==='drop_list')return {inbox:'/inbox',items:[{id:'d1',name:'report.txt',size:5,modified_unix_ms:Date.now()}]};
    if(req.action==='drop_get')return new Promise(r=>{resolveChunk=r;});
    return {};
   },
  });
  await df.flush();
  const dlPromise=df.byText('Download').action();
  df.abort.abort();
  resolveChunk({data:Buffer.from('hello').toString('base64'),size:5,eof:true});
  await dlPromise;
  assert.equal(shared,0,'MB-65: aborted download must not invoke navigator.share or save()');
 }
 console.log('PASS sweep W2 mobile regressions: MB-02, MB-24..27, MB-29..30, MB-48..56, MB-63, MB-65');
}
async function sweepR2MobileRegressions(){
 // R2M-01: Markdown & duplicate-question-line matching in answeredDecisionLines / multi-decision sheet.
 {
  const mdItem={
   uid:'item-r2m01-md',number:1,owner_bot_id:'b1',title:'MD Ask',status:'needs_you',
   events:[
    {id:10,kind:'status',actor:{bot:{bot_id:'b1'}},text:'status -> needs_you',decisions:[
     {question:'**Deploy target?**',options:['Staging','Prod'],recommended:0,multi:false},
     {question:'# `Region?`',options:['US','EU'],recommended:0,multi:false},
    ],created_at_ms:1000},
    // Historical raw-Markdown answer for Q0 must still match on read (back-compat).
    {id:11,kind:'commented',actor:{owner:{device:'desktop'}},text:'**Deploy target?** → Staging',decisions:[],created_at_ms:1100},
   ],
  };
  const dupItem={
   uid:'item-r2m01-dup',number:2,owner_bot_id:'b1',title:'Dup Ask',status:'needs_you',
   events:[
    {id:20,kind:'status',actor:{bot:{bot_id:'b1'}},text:'status -> needs_you',decisions:[
     {question:'**Pick option?**',options:['A','B'],recommended:0,multi:false},
     {question:'**Pick option?**',options:['C','D'],recommended:0,multi:false},
    ],created_at_ms:2000},
    {id:21,kind:'commented',actor:{owner:{device:'mobile'}},text:'Pick option? → A',decisions:[],created_at_ms:2100},
    {id:22,kind:'commented',actor:{owner:{device:'mobile'}},text:'Pick option? → C',decisions:[],created_at_ms:2200},
   ],
  };
  const uf=fixture('up-next',{
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'B1',agent_id:'claude'}]};
    if(req.action==='bots.items')return {items:[mdItem,dupItem]};
    return {requests:[],suggestions:[]};
   },
  });
  await uf.flush();
  // On mdItem: Q0 is answered via historical raw Markdown, so card surfaces Q1 ("Region?") options US/EU.
  assert(uf.root.textContent.includes('US')&&uf.root.textContent.includes('EU'),'R2M-01: raw Markdown historical answer marks Q0 answered so Q1 is active');
  // On dupItem: BOTH identical-question decisions are answered -> no C/D buttons left open.
  assert(!uf.all().some(b=>b.tagName==='BUTTON'&&b.getAttribute('aria-label')==='C'),'R2M-01: duplicate question line slot 1 recognized as answered');
  uf.abort.abort();
 }

 // R2M-25: Detail sheet refreshes snooze button label & click payload/toast after partial answer unsnoozes item.
 {
  const toasts=[];
  const snoozedMulti={
   uid:'item-r2m25',number:3,owner_bot_id:'b1',title:'Snoozed Multi',status:'needs_you',
   snoozed_until_ms:Date.now()+3600*1000,
   events:[
    {id:30,kind:'status',actor:{bot:{bot_id:'b1'}},text:'status -> needs_you',decisions:[
     {question:'Q1?',options:['A1','A2'],recommended:0,multi:false},
     {question:'Q2?',options:['B1','B2'],recommended:0,multi:false},
    ],created_at_ms:1000},
   ],
  };
  const uf=fixture('up-next',{
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'B1',agent_id:'claude'}]};
    if(req.action==='bots.items')return {items:[snoozedMulti]};
    if(req.action==='bots.item.answer'){
     return {
      item:{
       ...snoozedMulti,
       snoozed_until_ms:null,
       events:[...snoozedMulti.events,{id:31,kind:'commented',actor:{owner:{device:'mobile'}},text:'Q1? → A1',decisions:[],created_at_ms:2000}],
      },
     };
    }
    return {requests:[],suggestions:[]};
   },
  });
  uf.ctx.toast=m=>toasts.push(m);
  let closeCount=0;
  uf.ctx.closeSheet=()=>{closeCount++;};
  await uf.flush();
  // Open detail sheet via Details button.
  const detailsBtn=uf.all().find(b=>b.tagName==='BUTTON'&&String(b.textContent||'').includes('Details'));
  assert(detailsBtn,'R2M-25: Details button present');
  detailsBtn.action();
  const sheet=uf.sheets.at(-1);
  const body=uf.ctx.el('div');
  sheet[3](body);
  const findBtn=lbl=>body.querySelectorAll('button').find(b=>b.textContent===lbl||b.getAttribute('aria-label')===lbl);
  assert(findBtn('Unsnooze'),'R2M-25: starts with Unsnooze label when item is snoozed');
  await findBtn('A1').action();
  assert.equal(closeCount,0,'R2M-25: sheet stays open after partial answer');
  const snoozeNow=findBtn('Snooze 4h');
  assert(snoozeNow,'R2M-25: snooze button refreshes to Snooze 4h after partial answer clears snoozed_until_ms');
  assert.equal(snoozeNow.getAttribute('aria-label'),'Snooze 4h');
  await snoozeNow.action();
  const upd=uf.calls.find(c=>c.action==='bots.item.update'&&c.item_id==='item-r2m25');
  assert(upd&&typeof upd.snoozed_until_ms?.set==='number','R2M-25: clicking refreshed Snooze 4h sends set payload');
  assert(toasts.at(-1)==='Snooze 4h','R2M-25: toast matches live action label Snooze 4h');
  uf.abort.abort();
 }

 // R2M-26: Switching To Do filter or Bots Hub host when all target hosts fail clears stale arrays & cursors, and All Open filters client-side.
 {
  let failItems=false;
  const uf=fixture('up-next',{
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[{id:'b1',name:'B1',agent_id:'claude'}]};
    if(req.action==='bots.items'){
     if(failItems)return Promise.reject(Error('host down'));
     if(req.status==='closed')return {items:[{uid:'c1',number:9,owner_bot_id:'b1',title:'Closed Relic',status:'done',events:[]}],next_cursor:'cur-closed-1'};
     return {items:[]};
    }
    return {requests:[],suggestions:[]};
   },
  });
  await uf.flush();
  const closedBtn=uf.all().find(b=>b.tagName==='BUTTON'&&b.dataset.filter==='closed');
  await closedBtn.action();
  await uf.flush();
  assert(uf.root.textContent.includes('Closed Relic'),'R2M-26: closed item shown under Closed filter');
  assert(uf.all().some(b=>b.tagName==='BUTTON'&&b.textContent==='Load more'),'R2M-26: Load more present with cursor');
  // Switch to All Open while host fails -> closed relic & cursor MUST disappear.
  failItems=true;
  const openBtn=uf.all().find(b=>b.tagName==='BUTTON'&&b.dataset.filter==='open');
  await openBtn.action();
  await uf.flush();
  assert(!uf.root.textContent.includes('Closed Relic'),'R2M-26: closed item cleared when switching to All Open and host fails');
  assert(!uf.all().some(b=>b.tagName==='BUTTON'&&b.textContent==='Load more'),'R2M-26: stale cursor cleared on filter switch failure');
  uf.abort.abort();

  // Bots Hub deviceFilter switch to failing host clears previous host's bots.
  let activeHost='h1';
  const bf=fixture('bots',{state:{snapshot:{capabilities:{bots:true}}}});
  const conns=[
   {id:'h1',name:'Good PC',active:true,connection:{phase:'ready'},snapshot:{capabilities:{bots:true}}},
   {id:'h2',name:'Dead PC',active:false,connection:{phase:'ready'},snapshot:{capabilities:{bots:true}}},
  ];
  bf.ctx.connections=conns;
  bf.ctx.filteredConnections=()=>conns.filter(c=>c.id===activeHost);
  bf.ctx.rpcForHost=(hid,req)=>{
   if(hid==='h2')return Promise.reject(Error('offline'));
   if(req.action==='bots.list')return Promise.resolve({bots:[{id:'b1',name:'Good Bot',agent_id:'claude'}]});
   if(req.action==='bots.items')return Promise.resolve({items:[]});
   return Promise.resolve({requests:[],statuses:[],groups:[],sessions:[]});
  };
  bf.view.update();
  await bf.flush();
  assert(bf.root.textContent.includes('Good Bot'),'R2M-26: initial host bot rendered');
  activeHost='h2';
  bf.view.update();
  await bf.flush();
  assert(!bf.root.textContent.includes('Good Bot'),'R2M-26: stale host bots cleared when switching deviceFilter to failing host');
  bf.abort.abort();
 }

 // R2M-27: Bot Detail with 0 active global needs_you items and Include snoozed ON does not leak snoozed items into Bot Rail badge/sort.
 {
  const future=Date.now()+3600*1000;
  let failGlobal=false;
  const df=fixture('bots',{
   route:{botId:'b1'},
   state:{snapshot:{capabilities:{bots:true}}},
   rpc:req=>{
    if(req.action==='bots.list')return {bots:[
     {id:'b1',name:'Snooze Bot',agent_id:'claude'},
     {id:'b2',name:'Other Bot',agent_id:'claude'},
    ]};
    if(req.action==='bots.items'&&req.bot_id==='b1'){
     return {items:[{uid:'s1',number:1,owner_bot_id:'b1',title:'Snoozed Ask',status:'needs_you',snoozed_until_ms:future,events:[]}]};
    }
    if(req.action==='bots.items'&&!req.bot_id){
     if(failGlobal)return Promise.reject(Error('global fail'));
     return {items:[]};
    }
    if(req.action==='bots.connectors.status')return {statuses:[]};
    if(req.action==='bots.suggestions.list')return {suggestions:[]};
    if(req.action==='bots.access.list')return {requests:[]};
    if(req.action==='bots.sessions.list')return {sessions:[]};
    if(req.action==='bots.groups.list')return {groups:[]};
    return {};
   },
  });
  await df.flush();
  assert.equal(df.all().filter(n=>String(n.className||'').includes('bot-rail-badge')).length,0,'R2M-27: Bot Rail omits badge when global needs_you is empty even if bot items contain snoozed needs_you');
  // Even when global needs_you RPC fails and falls back to per-bot items, renderBotRail/sortBotsForDisplay exclude actively snoozed items.
  failGlobal=true;
  await df.byText('Refresh').action();
  await df.flush();
  assert.equal(df.all().filter(n=>String(n.className||'').includes('bot-rail-badge')).length,0,'R2M-27: Bot Rail excludes actively snoozed needs_you even on global fallback');
  df.abort.abort();
 }
 console.log('PASS sweep R2 mobile screen regressions: R2M-01, R2M-25, R2M-26, R2M-27');
}
async function main(){pathLabelSnapshots();await sessionDetailsAudit();exitedSessionProjection();await layoutAnchors();await tabletRoutePreservation();await tabBarAndLayoutContracts();await mobileAuditPairingAndShell();await installHandoffFragments();await mobileAuditIdentity();await mobileAuditScanner();await renderPerformance();await orchestration();await confirmationInbox();await workspace();await workspaceTree();await emptyWorkspace();await desktopWorkspaceOrder();await desktopVisibility();await projectFilters();await createWorktreeSheet();await projectIcons();await home();await homeResumeAndLogins();await git();await accounts();await settings();await quickCommandSettings();await installedQuickCommandSettings();await dropScreen();await botsScreens();await mobileAuditWave2Regressions();await todoUxDacRegressions();await voicePipitoRegressions();await sweepW2MobileRegressions();await sweepR2MobileRegressions();console.log('PASS mobile_screens: isolated screen regressions (no real browser/device layout assertion)');if(process.env.KRILL_SCREEN_I18N_KEYS)fs.writeFileSync(process.env.KRILL_SCREEN_I18N_KEYS,JSON.stringify([...copyKeys].filter(Boolean).sort(),null,2)+'\n');}
if(require.main===module)main().catch(error=>{console.error(error);process.exitCode=1;});module.exports={fixture,main};
