// Quick Commands: async ownership, search, persistence and single-send regressions.
'use strict';
const assert=require('node:assert/strict');
const flush=async()=>{for(let n=0;n<20;n++)await Promise.resolve();};
const deferred=()=>{let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};};
async function main(makeFixture=require('./mobile_session.cjs').fixture){
 const f=makeFixture([{id:7,agent:'codex',state:'Working'}],{quick_commands:[]}),{ctx,buttons,sheets,state}=f;
 ctx.act=fn=>fn();
 const catalog=deferred(),saves=[],prompts=[];let pendingSave,pendingPrompt,body;
 ctx.rpc=async request=>{if(request.action==='command_catalog')return catalog.promise;if(request.action==='set_quick_commands'){saves.push(request.commands);if(pendingSave)await pendingSave.promise;return {quick_commands:request.commands};}if(request.action==='prompt'){prompts.push(request);if(pendingPrompt)await pendingPrompt.promise;return {sent:true};}throw Error('unexpected RPC');};
 function render(){if(body)body.isConnected=false;const container=ctx.el('div');sheets.at(-1).render(container);body=container.children[0];body.isConnected=true;return body;}
 const find=cls=>body.descendants().find(n=>n.className===cls),all=cls=>body.descendants().filter(n=>n.className===cls);
 buttons.find(b=>b.className==='tab-quick').action();render();find('secondary quick-command-add').action();render();
 assert.equal(find('muted quick-command-hint').textContent,'Loading skills and commands…');
 find('quick-command-search').value='';
 catalog.resolve({supported:true,items:[{name:'inspect',text:'$inspect',description:'Inspect 한국어 code',kind:'Skill',scope:'Project'},{name:'review',text:'/prompts:review',description:'Review code',kind:'Command',scope:'Global'}]});await flush();
 assert.equal(all('quick-command-result').length,2);
 const search=find('quick-command-search');search.value='한국어 inspect';search.dispatchEvent(new Event('input'));assert.equal(all('quick-command-result').length,1);assert.equal(find('quick-command-name').textContent,'$inspect');
 pendingSave=deferred();const row=all('quick-command-result')[0];assert.equal(row.getAttribute('aria-label'),'Add $inspect');const one=row.action(),two=row.action();assert.equal(saves.length,1);pendingSave.resolve();await Promise.all([one,two]);await flush();assert.equal(state.snapshot.quick_commands[0].text,'$inspect');assert.equal(all('quick-command-result')[0].disabled,true);assert.equal(all('quick-command-result')[0].getAttribute('aria-label'),'$inspect · Added');
 search.value='no-match';search.dispatchEvent(new Event('input'));assert.equal(all('quick-command-result').length,0);assert.equal(find('muted quick-command-hint').textContent,'No skills or commands match.');
 // Replaced/closed sheets cannot reopen themselves when a write finally completes.
 search.value='review';search.dispatchEvent(new Event('input'));pendingSave=deferred();const adding=all('quick-command-result')[0].action();const before=sheets.length;body.isConnected=false;pendingSave.resolve();await adding;assert.equal(sheets.length,before);pendingSave=null;
 buttons.find(b=>b.className==='tab-quick').action();render();pendingPrompt=deferred();const run=all('quick-command-run')[0],sending=run.action();await run.action();assert.equal(prompts.length,1);assert.equal(prompts[0].session,7);assert.equal(prompts[0].text,'$inspect');pendingPrompt.resolve();await sending;
 // Editing and deletion preserve unrelated commands and never use a stale row index.
 assert.equal(find('icon-button quick-command-edit').getAttribute('aria-label'),'Edit: inspect');
 assert.equal(find('icon-button quick-command-delete').getAttribute('aria-label'),'Delete: inspect');
 find('icon-button quick-command-edit').action();render();const name=find('quick-command-name-input'),text=find('quick-command-text-input'),form=find('quick-command-form');name.value='Edited';text.value='$inspect --detail';form.dispatchEvent(new Event('submit',{cancelable:true}));await flush();render();assert.equal(state.snapshot.quick_commands[0].name,'Edited');
 assert.equal(find('icon-button quick-command-edit').getAttribute('aria-label'),'Edit: Edited');
 assert.equal(find('icon-button quick-command-delete').getAttribute('aria-label'),'Delete: Edited');
 const deleted=all('icon-button quick-command-delete')[0];state.snapshot.quick_commands.unshift({name:'Inserted elsewhere',text:'elsewhere'});await deleted.action();render();assert.equal(state.snapshot.quick_commands.length,2);assert.equal(state.snapshot.quick_commands[0].text,'elsewhere');assert(!state.snapshot.quick_commands.some(c=>c.name==='Edited'));
 f.view.cleanup();
 // A catalog response after navigation/disposal cannot create actionable results.
 const g=makeFixture([{id:7,agent:'claude'}],{quick_commands:[]}),late=deferred();g.ctx.rpc=()=>late.promise;g.buttons.find(b=>b.className==='tab-quick').action();const container=g.ctx.el('div');g.sheets.at(-1).render(container);g.buttons.find(b=>b.className==='secondary quick-command-add').action();const picker=g.ctx.el('div');g.sheets.at(-1).render(picker);g.view.cleanup();late.resolve({items:[{name:'late',text:'/late',kind:'Skill',scope:'Global'}]});await flush();assert(!picker.descendants().some(n=>n.className==='quick-command-result'));
 console.log('PASS quick commands: CLI search/filter, add/dedupe, persisted edit/delete, double-send and stale sheet/navigation guards');
}
if(require.main===module)main().catch(error=>{console.error(error);process.exitCode=1;});
module.exports={main};
