// Real Chromium touch regression: node crates/krilld/tests/mobile_scroll_browser.cjs
// Requires agent-browser and Node with WebSocket support. No daemon, PTY, or Rust build.
// Optional MOBILE_SCROLL_EVIDENCE_DIR retains measurements and screenshots locally.
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),os=require('node:os'),http=require('node:http');
const {execFile}=require('node:child_process'),run=require('node:util').promisify(execFile);
const pause=ms=>new Promise(resolve=>setTimeout(resolve,ms));
async function main(){
 const assets=path.join(__dirname,'../src/mobile/assets'),scratch=fs.mkdtempSync(path.join(os.tmpdir(),'krill-scroll-')),session=`krill-scroll-${process.pid}`,evidence=process.env.MOBILE_SCROLL_EVIDENCE_DIR;
 const browser=async(...args)=>(await run('agent-browser',['--session',session,...args],{timeout:45000})).stdout.trim();
 let server,ws;const pending=new Map(),results=[];let nextId=0;
 try{
  await run('node',[path.join(__dirname,'mobile_terminal.cjs'),'--harness'],{env:{...process.env,MOBILE_TERMINAL_HARNESS_DIR:scratch}});
  // Share the real adapter and wire fixture with the standard Cargo regression.
  const fixture=fs.readFileSync(path.join(scratch,'fixture.js'),'utf8').split(' globalThis.handle=')[0]+`
globalThis.setup=async(flags,fit,history,height)=>{
 globalThis.handle?.unmount();globalThis.handle=mountFixture(document.querySelector('main'),7,{preferences:{fitToPhone:false}});await Promise.resolve();
 globalThis.socket=sockets.at(-1);socket.handshake();socket.screen();await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));
 globalThis.port=document.querySelector('.terminal-scroll');globalThis.canvas=document.querySelector('.terminal-grid');
 const req=socket.sent.filter(a=>a[4]===0x12).at(-1),T=MkTerminalCodec,C=MkCrypto,r=T.reader(req.slice(5));
 const row=C.concat(T.v(1),T.text('x'),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0));
 socket.frame(0x26,T.v(r.n()),T.v(history),T.v(0),T.v(0),T.v(history),...Array(history).fill(row));
 if(fit){const p=handle.fitToPhone(true);socket.frame(0x2c,T.v(0));await p;}
 socket.frame(0x2b,Uint8Array.of(0),T.v(flags));if(height)port.style.maxHeight=height+'px';
 await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));port.scrollTop=port.scrollHeight;await new Promise(r=>setTimeout(r,40));
 globalThis.trace=[];for(const type of ['touchstart','touchmove','touchend','touchcancel','pointercancel'])port.addEventListener(type,e=>trace.push({type,trusted:e.isTrusted,cancelable:e.cancelable,prevented:e.defaultPrevented}),{passive:true});
};
globalThis.snapshot=()=>({top:port.scrollTop,height:port.scrollHeight,client:port.clientHeight,action:getComputedStyle(port).touchAction,trace,
 inputs:socket.sent.filter(a=>a[4]===0x13).map(a=>{const r=MkTerminalCodec.reader(a.slice(5));r.n();return r.text();})});`;
  const html='<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/app.css"><div id="mobile-app"><div class="session screen"><main class="terminal-mount"></main></div></div><script src="/fixture.js"></script>';
  server=http.createServer((req,res)=>{res.setHeader('Content-Type',req.url==='/app.css'?'text/css':req.url==='/fixture.js'?'text/javascript':'text/html');res.end(req.url==='/app.css'?fs.readFileSync(path.join(assets,'app.css')):req.url==='/fixture.js'?fixture:html);});
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));const origin=`http://127.0.0.1:${server.address().port}`;
  await browser('set','viewport','390','844');await browser('open',origin);
  // agent-browser owns the isolated browser; CDP supplies trusted touch input,
  // which dispatchEvent(new Event(...)) cannot exercise in the fake DOM harness.
  ws=new WebSocket(await browser('get','cdp-url'));await new Promise((resolve,reject)=>{ws.onopen=resolve;ws.onerror=reject;});
  ws.onmessage=e=>{const msg=JSON.parse(e.data),p=pending.get(msg.id);if(!p)return;pending.delete(msg.id);clearTimeout(p.timer);msg.error?p.reject(Error(JSON.stringify(msg.error))):p.resolve(msg.result);};
  const send=(method,params={},sessionId)=>new Promise((resolve,reject)=>{const id=++nextId,timer=setTimeout(()=>{pending.delete(id);reject(Error(`CDP timeout: ${method}`));},15000);pending.set(id,{resolve,reject,timer});ws.send(JSON.stringify({id,method,params,sessionId}));});
  const {targetInfos}=await send('Target.getTargets'),page=targetInfos.find(t=>t.type==='page'&&t.url.startsWith(origin));assert(page);
  const {sessionId}=await send('Target.attachToTarget',{targetId:page.targetId,flatten:true}),call=(method,params)=>send(method,params,sessionId);
  const evaluate=async expression=>{const result=await call('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});assert(!result.exceptionDetails,JSON.stringify(result.exceptionDetails));return result.result.value;};
  await call('Emulation.setTouchEmulationEnabled',{enabled:true,maxTouchPoints:5});
  async function drag(point,delta){
   await call('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[point]});
   const steps=Math.ceil(Math.abs(delta)/8),sign=Math.sign(delta);
   // Begin below the JS axis threshold, then cross it with real browser input.
   for(const distance of [2,4,...Array.from({length:steps},(_,i)=>Math.min(Math.abs(delta),(i+1)*8))]){
    await pause(20);await call('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:point.x,y:point.y+sign*distance}]});
   }
   await pause(120);await call('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});await pause(80);
  }
  const point=async area=>evaluate(`(()=>{const p=port.getBoundingClientRect(),c=canvas.getBoundingClientRect();return {x:p.x+p.width/2,y:${area==='history'?'p.y+60':'Math.max(p.y,c.y)+40'}}})()`);
  async function record(name,before,at){const after=await evaluate('snapshot()');assert(after.trace.some(t=>t.type==='touchmove'&&t.trusted),'requires trusted browser input');results.push({name,before,point:at,after});return after;}
  for(const flags of [0,20]){
   await evaluate(`setup(${flags},false,100,0)`);const before=await evaluate('snapshot()'),at=await point('history');
   assert.match(await evaluate(`document.elementFromPoint(${at.x},${at.y}).className`),/terminal-history/);
   await drag(at,96);const after=await record(`history-native-${flags}`,before,at);assert(after.top<before.top-50,'history must move even while the inline TUI has mouse mode');assert.equal(after.inputs.length,0,'history never sends TUI input');
  }
  for(const fit of [false,true])for(const flags of [20,84,192,193])for(const gap of [1,8,320]){
   await evaluate(`setup(${flags},${fit},${flags===20?100:0},${432+gap})`);const before=await evaluate('snapshot()'),at=await point('live');
   await drag(at,96);const up=await evaluate('snapshot()');await drag(at,-96);const after=await record(`short-${flags}-${fit}-${gap}`,before,at),bytes=after.inputs.join('');
   assert.equal(after.top,before.top,'remote scroll leaves local offset fixed');assert(up.inputs.length>0,'short canvas sends upward scroll');
   if(flags&16){assert.match(bytes,/\x1b\[<64;/);assert.match(bytes,/\x1b\[<65;/);}else{assert(bytes.includes(flags&1?'\x1bOA':'\x1b[A'));assert(bytes.includes(flags&1?'\x1bOB':'\x1b[B'));}
   assert(after.trace.filter(t=>t.type==='touchmove').every(t=>t.cancelable&&t.prevented),'TUI owns the gesture from the first tiny move');
  }
  for(const fit of [false,true])for(const flags of [20,84]){
   await evaluate(`setup(${flags},${fit},${flags===20?100:0},200)`);const before=await evaluate('snapshot()'),at=await point('live');
   await drag(at,320);const after=await record(`edge-${flags}-${fit}`,before,at),bytes=after.inputs.join('');
   assert.equal(before.top-after.top,232,'consume only visible canvas before remote scrolling');assert.match(bytes,/^(\x1b\[<64;\d+;\d+M){6}$/,'88 remaining pixels produce six detents');
   assert(!after.trace.some(t=>t.type==='pointercancel'),'native browser never steals a routed gesture');
  }
  // Shift remains the existing escape hatch to scroll local history from live.
  await evaluate('(async()=>{await setup(20,false,100,0);handle.toggleShift(true);})()');const before=await evaluate('snapshot()'),at=await point('live');await drag(at,96);const after=await record('shift-local',before,at);assert.equal(after.inputs.length,0);assert(after.top<before.top,JSON.stringify({before,at,after}));
  if(evidence){fs.mkdirSync(evidence,{recursive:true});await browser('screenshot',path.resolve(evidence,'scroll.png'));}
  console.log(`PASS ${results.length} trusted-touch cases: native history hit testing, inline/ALT SGR, 1007 arrows, Fit on/off, 1/8/320px gaps, canvas-edge remainder and Shift bypass`);
 }finally{
  if(evidence){fs.mkdirSync(evidence,{recursive:true});fs.writeFileSync(path.join(evidence,'measurements.json'),JSON.stringify(results,null,2));}
  for(const p of pending.values()){clearTimeout(p.timer);p.reject(Error('CDP closed'));}pending.clear();ws?.close();
  await browser('close').catch(()=>{});if(server)await new Promise(resolve=>server.close(resolve));fs.rmSync(scratch,{recursive:true,force:true});
 }
}
main().catch(error=>{console.error(error);process.exitCode=1;});
