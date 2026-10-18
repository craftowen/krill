// Session screen regressions without a browser or a running app.
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
class Element extends EventTarget {
 constructor(tag,text,cls){super();this.tag=tag;this.textContent=text;this.className=cls;this.children=[];this.dataset={};this.attrs={};}
 append(...nodes){for(const n of nodes){n.parentElement=this;this.children.push(n);}}
 replaceChildren(...nodes){this.children=[];this.append(...nodes);}
 setAttribute(k,v){this.attrs[k]=v;}
 querySelector(selector){return this.children.flatMap(n=>[n,...n.descendants()]).find(n=>selector==='h1'?n.tag==='h1':selector==='[aria-selected=true]'&&n.attrs['aria-selected']==='true');}
 descendants(){return this.children.flatMap(n=>[n,...n.descendants()]);}
 scrollIntoView(){}
 remove(){if(this.parentElement)this.parentElement.children=this.parentElement.children.filter(n=>n!==this);}
}
function fixture(){
 const timers=new Map(),mounts=[],buttons=[],sheets=[],calls=[];let clock=0,id=0;
 const sandbox={AbortController,setTimeout:(fn,ms)=>{timers.set(++id,{fn,at:clock+ms});return id;},clearTimeout:id=>timers.delete(id),KrillMobile:{terminal:{mount(root,options){const h={options,unmount(){options.onStatus({phase:'disconnected',message:'Disconnected'});}};mounts.push(h);options.onStatus({phase:'connecting'});return h;}}}};
 vm.createContext(sandbox);vm.runInContext(fs.readFileSync(path.join(__dirname,'../src/mobile/assets/screens/session.js'),'utf8'),sandbox);
 const session={id:7,state:'Working',branch:null},group={key:'g',label:'Workspace',sessions:[session]},state={groups:new Map([['g',group]]),sessions:new Map([[7,session]]),connection:{phase:'ready'},preferences:{},drafts:new Map()};
 const el=(...args)=>new Element(...args),ctx={el,state,route:{groupKey:'g',sessionId:7},signal:new AbortController().signal,button(label,action,cls){const b=el('button',label,cls);b.action=action;buttons.push(b);return b;},header(){const h=el('header'),copy=el('div');copy.append(el('h1'));h.append(copy);return h;},icon:()=>el('svg'),longPress:()=>()=>{},groupRoute:()=>'',go(){},rerender(){},savePreferences(){},error(){},photo(){},transport:{},sheet:(title,entries)=>sheets.push(entries),rpc:async request=>calls.push(request),refresh:async()=>{}};
 const view=sandbox.KrillMobile.screens.session(el('main'),ctx);
 function advance(ms){clock+=ms;for(const [key,t] of [...timers])if(t.at<=clock){timers.delete(key);t.fn();}}
 return {view,state,ctx,mounts,timers,advance,buttons,sheets,calls};
}
async function main(){
 let f=fixture();assert.equal(f.mounts.length,1);f.mounts[0].options.onStatus({phase:'ready'});f.mounts[0].options.onStatus({phase:'disconnected'});for(let n=0;n<10;n++)f.view.update();assert.equal(f.timers.size,1);f.advance(999);assert.equal(f.mounts.length,1);f.advance(1);assert.equal(f.mounts.length,2);f.mounts[1].options.onStatus({phase:'disconnected'});f.advance(1999);assert.equal(f.mounts.length,2);f.advance(1);assert.equal(f.mounts.length,3);f.mounts[2].options.onStatus({phase:'disconnected'});f.view.cleanup();f.advance(60000);assert.equal(f.mounts.length,3);f.mounts[0].options.onStatus({phase:'disconnected'});assert.equal(f.timers.size,0);
 f=fixture();f.mounts[0].options.onStatus({phase:'disconnected'});f.state.connection.phase='disconnected';f.view.update();f.advance(60000);assert.equal(f.mounts.length,1);f.state.connection.phase='ready';f.view.update();assert.equal(f.mounts.length,2);f.view.cleanup();
 console.log('PASS terminal-only reconnect backoff, generation/cleanup and main connection cancellation');
}
if(require.main===module)main().catch(e=>{console.error(e);process.exitCode=1;});
module.exports={fixture,main};
