'use strict';
// Read-only isolated Node fixtures. No browser, network, daemon or PTY.
const fs=require('node:fs'),path=require('node:path');
const repo=process.argv[2]||'/path/to/krill';
process.chdir(repo);
const dir=path.join(repo,'crates/krilld/tests');
let terminal=fs.readFileSync(dir+'/mobile_terminal.cjs','utf8').split("if(process.argv.includes('--harness'))")[0];
terminal+=String.raw`
return (async()=>{
 let f=await mount(501),cap=find(f.root,'live-capture');f.handle.focus();cap.value='a';event(cap,'input');await flush();let r=T.reader(f.socket.sent.at(-1).slice(5)),id=r.n();f.socket.frame(0x27,T.v(id),T.v(1));await flush();cap.value='ab';event(cap,'input');await flush();console.log('AUDIT rejected a then type ab -> wire',JSON.stringify(inputs(f.socket)));f.handle.unmount();
 f=await mount(502);f.handle.focus();cap=find(f.root,'live-capture');cap.value='echo one\necho two';event(cap,'input',{inputType:'insertFromPaste'});await flush();console.log('AUDIT paste -> wire',JSON.stringify(inputs(f.socket)));f.handle.unmount();
 f=await mount(503);f.handle.setInputMode('buffered');let command=find(f.root,'command-input');command.value='echo hang';event(find(f.root,'buffered-command'),'submit');await flush();await advance(120000);console.log('AUDIT no ACK after 120s ->',JSON.stringify({disabled:find(f.root,'command-send').disabled,phase:f.root.dataset.phase,value:command.value}));f.handle.unmount();await flush();
})()`;
const prefix=fs.readFileSync(dir+'/mobile_session.cjs','utf8').split('const longPressCode')[0];
const chat=String.raw`
return (async()=>{
const flush=async()=>{for(let i=0;i<20;i++)await Promise.resolve();};
const find=(r,cls)=>[r,...r.descendants()].find(n=>n.className?.split(' ').includes(cls));
const buttons=[],calls=[],errors=[];let polls=[],resolvePrompt;
const state={connection:{phase:'ready'},drafts:new Map(),sessions:new Map([[7,{id:7,state:'Working',cwd:'/tmp/fixture',lines:['working']} ]])};
const ctx={state,el:(...a)=>new Element(...a),button(label,action,cls){const n=new Element('button',label,cls);n.action=action;buttons.push(n);return n;},rpc:async req=>{calls.push(req);if(req.action==='chat_poll')return polls.shift()||{items:[],next_offset:40,bound:true};if(req.action==='prompt')return await new Promise(r=>resolvePrompt=r);return {sent:true};},act:async fn=>{try{await fn();}catch(e){errors.push(e.message);}},refresh:async()=>{},photo:async()=>{throw Error('photo fail');}};
const sandbox={KrillMobile:{},setTimeout:()=>1,clearTimeout(){}};vm.createContext(sandbox);vm.runInContext(fs.readFileSync(path.resolve('crates/krilld/src/mobile/assets/screens/chat.js'),'utf8'),sandbox);
const q={kind:'question',tool_id:'q1',text:'Which?',multi_select:true,options:[{label:'A'},{label:'B'}]};
polls.push({bound:true,next_offset:10,items:[q,{kind:'permission',text:'Old approve?'}]});
const root=ctx.el('main'),handle=sandbox.KrillMobile.chat.mount(root,{ctx,sessionId:7});await flush();
let custom=find(root,'chat-question-custom').children[0];custom.value='my answer';buttons.find(b=>b.textContent==='A').action();
const oldCustom=custom;polls.push({bound:true,next_offset:20,items:[{kind:'assistant',text:'new delta'}]});handle.update();await flush();
custom=find(root,'chat-question-custom').children[0];console.log('AUDIT delta replaces custom field',custom!==oldCustom,'value=',custom.value||'','selection=',buttons.filter(b=>b.textContent==='A').at(-1).className);
await buttons.find(b=>b.textContent==='Approve (y)').action();await flush();console.log('AUDIT historical approval while Working ->',JSON.stringify(calls.filter(c=>c.action==='approve')));
let input=find(root,'chat-input');input.value='first';state.drafts.set(7,'first');find(root,'chat-composer').dispatchEvent(new Event('submit',{cancelable:true}));await flush();input.value='second draft';state.drafts.set(7,'second draft');resolvePrompt({sent:true});await flush();console.log('AUDIT edit pending send ->',JSON.stringify({value:input.value,draft:state.drafts.get(7)}));
const feed=find(root,'chat-feed'),before=feed.children.length;polls.push({reset:true,bound:false,next_offset:0,items:[]});handle.update();await flush();console.log('AUDIT empty reset cache/render',JSON.stringify({cache:state.chatCache.get(7).items.length,rendered:feed.children.length,before}));
handle.unmount();
})()`;
(async()=>{
 await new Function('require','__dirname',terminal)(require,dir);
 await new Function('require','__dirname',prefix+chat)(require,dir);
})().catch(e=>{console.error(e);process.exitCode=1;});
