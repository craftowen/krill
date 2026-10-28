// Removed Quick Commands must stay absent even when older daemons return saved commands.
'use strict';
const assert=require('node:assert/strict');
async function main(makeFixture=require('./mobile_session.cjs').fixture){
 for(const tablet of [false,true])for(const session_create of [false,true]){
  const f=makeFixture([{id:7,agent:'codex',state:'Working'}],{capabilities:{session_create},quick_commands:[{name:'Run tests',text:'cargo nextest run'}]},tablet);
  const check=root=>{const nodes=root.descendants();assert(!nodes.some(n=>n.className==='tab-quick'||n.className==='tab-action-divider'));assert(!f.buttons.some(b=>b.textContent==='Quick Commands'));assert.equal(f.calls.length,0);};
  check(f.root);f.state.connection.phase='disconnected';f.view.update();check(f.root);f.state.connection.phase='ready';f.view.update();check(f.root);f.view.cleanup();
  const group=f.state.groups.get('g');f.ctx.creating=()=>({group,agent:'codex'});f.ctx.route={groupKey:'g',sessionId:'new'};
  const pending=f.ctx.el('main'),view=f.sandbox.KrillMobile.screens.session(pending,f.ctx);check(pending);view.cleanup();
 }
 console.log('PASS removed Quick Commands stay absent in terminal and pending screens, on phone/tablet and reconnect with saved commands');
}
if(require.main===module)main().catch(error=>{console.error(error);process.exitCode=1;});
module.exports={main};
