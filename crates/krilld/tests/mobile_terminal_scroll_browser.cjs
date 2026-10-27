// Real Chromium touch scrolling with the production adapter/CSS. No user session or Rust build.
// Run: node crates/krilld/tests/mobile_terminal_scroll_browser.cjs [evidence-directory]
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),os=require('node:os'),path=require('node:path'),http=require('node:http');
const {execFile}=require('node:child_process'),{promisify}=require('node:util'),exec=promisify(execFile);
const assets=path.join(__dirname,'../src/mobile/assets'),session=`terminal-scroll-${process.pid}`;
const evidence=path.resolve(process.argv[2]||'/tmp/krill-mobile-terminal-scroll');
const browser=async(...args)=>(await exec('agent-browser',['--session',session,...args],{timeout:45000})).stdout;
const evaluate=async source=>JSON.parse(await browser('eval','--json',source)).data.result;

class Cdp {
 constructor(socket){this.socket=socket;this.next=0;this.pending=new Map();socket.addEventListener('message',event=>{const message=JSON.parse(event.data),job=this.pending.get(message.id);if(!job)return;this.pending.delete(message.id);clearTimeout(job.timer);message.error?job.reject(Error(message.error.message)):job.resolve(message.result);});}
 static async connect(url){const socket=new WebSocket(url);await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});return new Cdp(socket);}
 send(method,params={},sessionId){return new Promise((resolve,reject)=>{const id=++this.next,timer=setTimeout(()=>{this.pending.delete(id);reject(Error(`CDP timeout: ${method}`));},10000);this.pending.set(id,{resolve,reject,timer});this.socket.send(JSON.stringify({id,method,params,...(sessionId&&{sessionId})}));});}
 close(){this.socket.close();}
}

const fixture=`
globalThis.sentInputs=()=>socket.sent.filter(a=>a instanceof Uint8Array&&a[4]===0x13).map(a=>{const r=MkTerminalCodec.reader(a.slice(5));r.n();return r.text();}).join('');
globalThis.mountCase=async(fit=true,tall=false)=>{
 globalThis.handle?.unmount();globalThis.handle=mountFixture(document.querySelector('main'),7,{preferences:{inputMode:'direct',fitToPhone:false}});await Promise.resolve();globalThis.socket=sockets.at(-1);socket.handshake();socket.autoAck=true;
 const T=MkTerminalCodec,C=MkCrypto,send=socket.send.bind(socket);let position=100,revision=2;
 socket.send=data=>{send(data);if(!(data instanceof Uint8Array))return;if(data[4]===0x17)queueMicrotask(()=>socket.frame(0x2c,T.v(0)));if(data[4]===0x13){const r=T.reader(data.slice(5));r.n();const text=r.text();position-=(text.match(/\\x1b\\[<64;/g)||[]).length;position+=(text.match(/\\x1b\\[<65;/g)||[]).length;const chars=[...('History row '+position)],cell=ch=>C.concat(T.text(ch),T.v(0),T.v(0),T.v(0),T.v(0),T.v(0));socket.frame(0x21,T.v(++revision),T.v(revision),T.v(1),T.v(1),T.v(2),T.v(0),T.v(chars.length),...chars.map(cell));}};
 socket.screen('History row 100');const port=document.querySelector('.terminal-scroll'),rows=Math.max(10,Math.floor(port.clientHeight/18))*(tall?2:1);socket.frame(0x21,T.v(revision),T.v(revision),T.v(1),T.v(1),T.v(0),T.v(80),T.v(rows));socket.frame(0x2b,Uint8Array.of(0),T.v(120));if(fit)await handle.fitToPhone(true);
 globalThis.touchEvents=[];for(const type of ['touchstart','touchmove','touchend'])port.addEventListener(type,e=>touchEvents.push({type,trusted:e.isTrusted,prevented:e.defaultPrevented}));
 await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));port.scrollTop=port.scrollHeight;
};
globalThis.measure=()=>{const p=document.querySelector('.terminal-scroll'),c=document.querySelector('canvas');return {top:p.scrollTop,height:p.clientHeight,scrollHeight:p.scrollHeight,canvasHeight:c.getBoundingClientRect().height,up:(sentInputs().match(/\\x1b\\[<64;/g)||[]).length,down:(sentInputs().match(/\\x1b\\[<65;/g)||[]).length,text:handle.text(),touchEvents};};
globalThis.handle=null;mountCase();
`;

(async()=>{
 fs.mkdirSync(evidence,{recursive:true});const scratch=fs.mkdtempSync(path.join(os.tmpdir(),'krill-scroll-'));
 let server,cdp;
 try{
  await exec(process.execPath,[path.join(__dirname,'mobile_terminal.cjs'),'--harness'],{env:{...process.env,MOBILE_TERMINAL_HARNESS_DIR:scratch}});
  const source=fs.readFileSync(path.join(scratch,'fixture.js'),'utf8').split(' globalThis.handle=')[0]+fixture;
  const html='<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover"><link rel="stylesheet" href="/app.css"><div id="mobile-app"><div class="session screen"><main class="terminal-mount"></main></div></div><script src="/fixture.js"></script>';
  server=http.createServer((req,res)=>{const file=req.url==='/'?html:req.url==='/fixture.js'?source:req.url==='/app.css'?fs.readFileSync(path.join(assets,'app.css')):null;if(file===null){res.writeHead(404);res.end();return;}res.setHeader('Content-Type',req.url==='/'?'text/html':req.url.endsWith('.css')?'text/css':'text/javascript');res.end(file);});
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));const url=`http://127.0.0.1:${server.address().port}/`;
  await browser('set','device','iPhone 14');await browser('open',url);await browser('wait','--fn','document.querySelector("main").dataset.fit==="true"');
  cdp=await Cdp.connect((await browser('get','cdp-url')).trim());const {targetInfos}=await cdp.send('Target.getTargets'),target=targetInfos.find(t=>t.url===url),{sessionId}=await cdp.send('Target.attachToTarget',{targetId:target.targetId,flatten:true});
  const swipe=async(start,end)=>{await cdp.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{x:150,y:start}]},sessionId);for(let i=1;i<=12;i++){await cdp.send('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:150,y:start+(end-start)*i/12}]},sessionId);await new Promise(resolve=>setTimeout(resolve,10));}await cdp.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]},sessionId);};
  const results=[];
  for(const [width,height] of [[390,844],[844,390]])for(const fit of [false,true]){
   await browser('set','viewport',String(width),String(height));await evaluate(`mountCase(${fit})`);const before=await evaluate('measure()');assert(before.canvasHeight<before.height,'exercise a partial-row gap below the grid');
   await swipe(100,220);const up=await evaluate('measure()');assert(up.up>0,`up swipe swallowed: ${JSON.stringify({width,height,fit,before,up})}`);assert(up.text.includes('History row'),'the rendered terminal changes after its scroll report');
   await browser('screenshot',path.join(evidence,`${width}x${height}-${fit?'fit':'original'}-up.png`));
   await swipe(220,100);const down=await evaluate('measure()');assert(down.down>0,'reverse swipe reaches the TUI');assert(down.touchEvents.length>0&&down.touchEvents.every(e=>e.trusted),'use browser-generated touch events');assert.equal(down.top,0,'no negative DOM scroll range');
   await browser('screenshot',path.join(evidence,`${width}x${height}-${fit?'fit':'original'}.png`));results.push({width,height,fit,before,up,down});
  }
  await browser('set','viewport','390','844');await evaluate('mountCase(true,true)');const before=await evaluate('measure()');await swipe(100,220);const after=await evaluate('measure()');assert(after.top<before.top,'overflowing canvas still scrolls locally');assert.equal(after.up,0,'local canvas scrolling does not also scroll the TUI');
  fs.writeFileSync(path.join(evidence,'measurements.json'),JSON.stringify({results,overflow:{before,after}},null,2)+'\n');
  console.log('PASS trusted Chromium touch: short canvas, both directions, Fit/original, portrait/landscape, visible output and overflowing canvas');
 }catch(error){try{fs.writeFileSync(path.join(evidence,'failure.json'),JSON.stringify(await evaluate('measure()'),null,2));await browser('screenshot',path.join(evidence,'failure.png'));}catch{}throw error;
 }finally{cdp?.close();await browser('close').catch(()=>{});if(server)await new Promise(resolve=>server.close(resolve));fs.rmSync(scratch,{recursive:true,force:true});}
})().catch(error=>{console.error(error);process.exitCode=1;});
