/* Independent non-secure-context client against real krill-holder/krilld. */
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),os=require('node:os'),vm=require('node:vm'),crypto=require('node:crypto'),zlib=require('node:zlib');
const net=require('node:net');
const {spawn,execFileSync}=require('node:child_process'),{once}=require('node:events');
const daemonBin=path.resolve(process.argv[2]),bin=path.dirname(daemonBin),mk=path.join(bin,'krill'),assets=path.join(__dirname,'../src/mobile/assets');
const keep=process.argv.includes('--keep'),metadata=process.env.M9_BROWSER_METADATA||'/tmp/m9-browser-fixture.json';
const sandbox={TextEncoder,TextDecoder,Uint8Array,Uint32Array,DataView,BigInt,atob,crypto:{getRandomValues:bytes=>crypto.randomFillSync(bytes)},setTimeout,clearTimeout};
vm.createContext(sandbox);vm.runInContext(fs.readFileSync(path.join(assets,'app.js'),'utf8'),sandbox);const C=sandbox.MkCrypto;
assert.equal(sandbox.crypto.subtle,undefined);assert.equal(sandbox.crypto.randomUUID,undefined);
for(const n of [0,1,3,63,64,65,1024]){const bytes=crypto.randomBytes(n);assert.equal(C.hex(C.sha256(bytes)),crypto.createHash('sha256').update(bytes).digest('hex'));}
// Compare both directions to Node/OpenSSL, including block edges and a photo-sized frame.
for(const size of [0,1,15,16,17,63,64,65,1024,512*1024]){
 const secret=crypto.randomBytes(32),salt=crypto.randomBytes(64),client=new C.Channel(secret,salt),server=new C.Channel(secret,salt,false);
 for(const [sender,receiver,info,prefix] of [[client,server,'client-to-daemon','MKC2'],[server,client,'daemon-to-client','MKD2']]){
  const key=Buffer.from(crypto.hkdfSync('sha256',secret,salt,Buffer.from('mk-mobile-v2/'+info),32));
  for(let seq=0;seq<2;seq++){
   const plain=crypto.randomBytes(size),wire=Buffer.from(sender.seal(plain)),aad=Buffer.alloc(8);aad.writeBigUInt64LE(BigInt(seq));
   const cipher=crypto.createCipheriv('chacha20-poly1305',key,Buffer.concat([Buffer.from(prefix),aad]),{authTagLength:16});cipher.setAAD(aad);
   assert.deepEqual(wire,Buffer.concat([aad,cipher.update(plain),cipher.final(),cipher.getAuthTag()]));
   const tampered=Buffer.from(wire);tampered[tampered.length-1]^=1;assert.throws(()=>receiver.open(tampered),/Authentication/);
   assert.deepEqual(Buffer.from(receiver.open(wire)),plain);assert.throws(()=>receiver.open(wire),/Replay/);
  }
 }
}
const vector=new C.Channel(new Uint8Array(32).fill(7),new Uint8Array(64).fill(9));assert.equal(C.hex(vector.seal(new TextEncoder().encode('prompt'))),'0000000000000000ca8c87d414301b7fc3a6a3f09a5fb977fb9422ea97cb');
for(const name of ['index.html','app.js','app.css','manifest.json','icon.svg'])assert.deepEqual(zlib.gunzipSync(fs.readFileSync(path.join(assets,name+'.gz'))),fs.readFileSync(path.join(assets,name)));
assert.equal(fs.readFileSync(path.join(assets,'app.js'),'utf8'),['crypto.js','terminal.js','client.js'].map(name=>fs.readFileSync(path.join(assets,name),'utf8')).join(''));
// Cross-language fixed screen fixtures: wide Hangul, combining ZWJ, colors and scroll ops.
const T=sandbox.MkTerminalCodec,model={};
T.update(Buffer.from('3000000021ac02f0a204010400501801030203020161000000000003ed959c0103e2808d01090201020302ff0080e80503030201','hex').subarray(5),model);
assert.equal(model.cols,80);assert.equal(model.lines,24);assert.equal(model.rows[3][0].ch,'a');assert.equal(model.rows[3][1].ch,'한');assert.equal(model.rows[3][1].zero,'\u200d');assert.equal(model.cursor.column,2);
const historyFixture=T.history(Buffer.from('100000002609e807030a02010178000000000000','hex').subarray(5));assert.equal(historyFixture.total,1000);assert.equal(historyFixture.pages,3);assert.equal(historyFixture.rows[0][0].ch,'x');
const before=model.rows[3];T.update(Buffer.concat([v(301),v(70000),v(1),v(1),v(1),v(2)]),model);assert.equal(model.rows[2],before);
for(let n=0;n<128;n++){const bytes=crypto.randomBytes(n);try{T.update(bytes,{cols:80,lines:24,rows:Array.from({length:24},()=>[])});}catch{}try{T.history(bytes);}catch{}}
assert.deepEqual(Array.from(T.keys,([label,bytes])=>[label,Buffer.from(bytes).toString('hex')]),[['Esc','1b'],['Tab','09'],['⇧Tab','1b5b5a'],['Ctrl+C','03'],['Ctrl+D','04'],['↑','1b5b41'],['↓','1b5b42'],['←','1b5b44'],['→','1b5b43'],['Enter','0d']]);
const dir=fs.mkdtempSync('/tmp/m9pwa-'),repo=path.join(dir,'repo'),home=path.join(dir,'home'),profile=path.join(dir,'account');
fs.mkdirSync(repo);fs.mkdirSync(home);fs.mkdirSync(profile);fs.mkdirSync(path.join(dir,'paste'),{mode:0o700});
const env={...process.env,KRILL_STATE_DIR:dir,HOME:home,TMPDIR:path.join(dir,'paste')};
const log=fs.openSync(path.join(dir,'holder.log'),'a');let holder,daemon=0,phone,other,webhookServer,files=[];
const deadline=setTimeout(()=>{console.error('PWA E2E deadline exceeded');cleanup();process.exit(1);},90_000);
function cli(args){return execFileSync(mk,args,{env,encoding:'utf8',timeout:20000,stdio:['ignore','pipe','pipe']}).trim();}
function jsonCli(args){return JSON.parse(cli([...args,'--json']));}
function git(args){return execFileSync('git',args,{cwd:repo,env,encoding:'utf8'}).trim();}
function v(value){let n=BigInt(value),out=[];do{let b=Number(n&127n);n>>=7n;out.push(b|(n?128:0));}while(n);return Buffer.from(out);}
function string(s){const bytes=Buffer.from(s);return Buffer.concat([v(bytes.length),bytes]);}
function frame(k,body){const h=Buffer.alloc(5);h.writeUInt32LE(body.length+1);h[4]=k;return Buffer.concat([h,body]);}
function reader(bytes){let at=0;return{u(){let n=0n,shift=0n;for(;;){const b=bytes[at++];n|=BigInt(b&127)<<shift;if(!(b&128))return n;shift+=7n;}},text(){const n=Number(this.u()),s=bytes.subarray(at,at+n).toString();at+=n;return s;}};}
async function until(fn){const end=Date.now()+15000;while(Date.now()<end){try{const value=await fn();if(value)return value;}catch{}await new Promise(r=>setTimeout(r,30));}throw Error('condition timed out');}
function cleanup(){clearTimeout(deadline);webhookServer?.close();phone?.socket.close();other?.socket.close();if(holder){try{const list=cli(['ls']).split('\n');for(const line of list){const id=line.split('\t')[0];if(/^\d+$/.test(id))cli(['kill',id]);}}catch{}holder.kill('SIGKILL');}if(daemon)try{process.kill(daemon,'SIGKILL');}catch{}for(const f of files)try{fs.unlinkSync(f);}catch{}fs.closeSync(log);fs.rmSync(dir,{recursive:true,force:true});}
async function connect(url,credential){
 const u=new URL(url),socket=new WebSocket(`ws://${u.host}/ws`);socket.binaryType='arraybuffer';const messages=[],waiting=[];let failure;
 socket.addEventListener('message',e=>{const w=waiting.shift();if(w)w.resolve(e.data);else messages.push(e.data);});
 socket.addEventListener('close',()=>{failure=Error('WS closed');for(const w of waiting.splice(0))w.reject(failure);});
 const next=()=>messages.length?Promise.resolve(messages.shift()):failure?Promise.reject(failure):new Promise((resolve,reject)=>waiting.push({resolve,reject}));
 await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});
 const challenge=JSON.parse(await next()),nonce=crypto.randomBytes(32),secret=credential?C.sha256(C.unhex(credential.credential)):C.unbase64(new URLSearchParams(u.hash.slice(1)).get('key'));
 const channel=new C.Channel(secret,C.concat(C.unhex(challenge.challenge),nonce));
 socket.send(JSON.stringify({token:credential?null:new URLSearchParams(u.hash.slice(1)).get('pair'),device_id:credential?credential.device_id:null,client_nonce:nonce.toString('hex')}));
 socket.send(channel.seal(new TextEncoder().encode('{"label":"Non-secure HTTP test"}')));
 const authenticated=JSON.parse(new TextDecoder().decode(channel.open(await next()))),hello=Buffer.from(channel.open(await next())),r=reader(hello.subarray(5));r.u();r.u();const features=r.u();r.text();daemon=Number(r.u());
 socket.send(channel.seal(frame(2,Buffer.concat([v(1),v(1),v(features),v(0),string('pure-js-e2e')]))));
 let serial=Promise.resolve();
 return{socket,authenticated,async rpc(request){const operation=serial.then(async()=>{socket.send(channel.seal(frame(0x45,Buffer.concat([v(7),string(JSON.stringify(request))]))));const bytes=Buffer.from(channel.open(await next()));assert.equal(bytes[4],0x65);const r=reader(bytes.subarray(5));assert.equal(r.u(),6n);const response=JSON.parse(r.text());if(!response.ok)throw Error(response.error);return response.data;});serial=operation.catch(()=>{});return operation;}};
}
(async()=>{
 try {
  git(['init','-b','mobile-test']);git(['config','user.email','test@example.invalid']);git(['config','user.name','M9 test']);fs.writeFileSync(path.join(repo,'example.txt'),'before\n');git(['add','.']);git(['commit','-m','fixture']);fs.appendFileSync(path.join(repo,'example.txt'),'after\n');
  fs.writeFileSync(path.join(profile,'statusline_cache.json'),JSON.stringify({five_hour:{utilization:23},seven_day:{utilization:47}}));
  fs.writeFileSync(path.join(dir,'accounts.toml'),`[active]\nclaude = "claude-host"\n[[accounts]]\nid = "mobile-test-account"\nprovider = "claude"\nlabel = "Mobile fixture"\nprofile_dir = ${JSON.stringify(profile)}\n`);
  fs.writeFileSync(path.join(dir,'agents.toml'),'[agent.codex]\nname = "Mock Codex"\nlaunch = ["/bin/sh", "-c", "printf \'mock-agent-ready\\\\n\'; exec /bin/cat"]\n[agent.claude]\nname = "Mock Claude"\nlaunch = ["/bin/sh", "-c", "printf \'mock-agent-ready\\\\n\'; exec /bin/cat"]\n');
  fs.writeFileSync(path.join(dir,'waiting.json'),'{"hook_event_name":"PermissionRequest"}');
  holder=spawn(path.join(bin,'krill-holder'),[daemonBin,path.join(dir,'run/krilld.sock')],{env,stdio:['ignore',log,log]});await until(()=>fs.existsSync(path.join(dir,'run/krilld.sock')));
  daemon=await new Promise((resolve,reject)=>{const socket=net.createConnection(path.join(dir,'run/krilld.sock'));let bytes=Buffer.alloc(0);socket.setTimeout(5000,()=>socket.destroy(Error('hello timeout')));socket.on('error',reject);socket.on('data',part=>{bytes=Buffer.concat([bytes,part]);if(bytes.length>=4&&bytes.length>=4+bytes.readUInt32LE()){const r=reader(bytes.subarray(5));r.u();r.u();r.u();r.text();resolve(Number(r.u()));socket.destroy();}});});
  const shell=`stty -echo; sleep 0.15; '${mk}' hook claude <'${dir}/waiting.json' >/dev/null; printf 'READY\\n'; while IFS= read -r line; do printf 'got:%s\\n' "$line"; done`;
  const session=Number(cli(['new','--cwd',repo,'--','/bin/sh','-c',shell]));await until(()=>cli(['read',String(session)]).includes('READY'));
  const run=jsonCli(['orchestration','run-create','--title','Mobile run']);const runId=run.id||run.run_id||run.run?.id;assert(runId,JSON.stringify(run));
  const task=jsonCli(['orchestration','task-create','--run',runId,'--title','Mobile task']);
  const question=jsonCli(['orchestration','ask','--from','mobile-worker','--to','coordinator','--question','Proceed with fixture?']);
  const gate=jsonCli(['orchestration','gate-create','--from','test-worker','--question','Deploy fixture?','--options','Approve,Deny','--run',runId]);const gateId=gate.id||gate.gate_id;assert(gateId,JSON.stringify(gate));
  const offer=jsonCli(['mobile','pair','--address',keep?(process.env.M9_BROWSER_ADDRESS||'127.0.0.1:0'):'127.0.0.1:0']);const url=offer.PairingOffer.url,base=new URL(url).origin;
  for(const name of ['/','/app.js','/app.css','/manifest.json','/icon.svg']){const response=await fetch(base+name);assert.equal(response.status,200);assert.equal(response.headers.get('content-encoding'),'gzip');assert((await response.arrayBuffer()).byteLength>0);}
  if(keep){let terminalSession=null;if(process.env.M9_LIVE_QA){
    const generator=path.join(dir,'terminal.py');
    fs.writeFileSync(generator,`import os, sys, tty, termios, fcntl, struct\nfor i in range(20000):\n print('%06d history 한글 ' % i + 'x'*110, flush=True)\nprint('\\x1b[31mLIVE RED\\x1b[0m 한글 e\\u0301', flush=True)\ntty.setraw(0)\nwhile True:\n data=os.read(0,4096)\n if not data: break\n rows,cols,_,_=struct.unpack('HHHH',fcntl.ioctl(0,termios.TIOCGWINSZ,b'\\0'*8))\n os.write(1,('\\r\\nKEYS:'+data.hex()+' SIZE:%dx%d\\r\\n'%(cols,rows)).encode())\n`);
    terminalSession=Number(cli(['new','--cwd',repo,'--','/usr/bin/python3','-u',generator]));
    for(let i=0;i<7;i++)cli(['new','--cwd',repo,'--','/bin/sh','-c',"printf 'IDLE\\n'; exec /bin/cat"]);
    await until(()=>cli(['read',String(terminalSession)]).includes('LIVE RED'));
  }clearTimeout(deadline);fs.writeFileSync(metadata,JSON.stringify({dir,repo,url,session,terminalSession,base,daemon,gateId,profile}));console.log('Browser fixture ready');await new Promise(r=>process.once('SIGTERM',r));return;}
  phone=await connect(url);let data=await phone.rpc({action:'snapshot'});const card=data.sessions.find(s=>s.id===session);assert(card);assert.equal(card.state,'Waiting');assert.equal(card.branch,'mobile-test');assert.equal(card.added,1);assert(card.lines.some(l=>l.includes('READY')));assert(data.orchestration.gates.some(g=>g.id===gateId));assert(data.orchestration.tasks.length>=1);assert(data.orchestration.questions.some(q=>q.id===question.id));
  await phone.rpc({action:'approve',session,value:'y'});await until(()=>cli(['read',String(session)]).includes('got:y'));
  await phone.rpc({action:'approve',session,value:'n'});await until(()=>cli(['read',String(session)]).includes('got:n'));
  await phone.rpc({action:'prompt',session,text:'안녕 모바일'});await until(()=>cli(['read',String(session)]).includes('got:안녕 모바일'));
  const photo=await phone.rpc({action:'photo',session,mime:'image/png',data:'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/a9sAAAAASUVORK5CYII='});files.push(photo.path);assert.equal(fs.statSync(photo.path).mode&0o777,0o600);assert.equal(fs.readFileSync(photo.path).subarray(0,8).toString('hex'),'89504e470d0a1a0a');await phone.rpc({action:'prompt',session,text:''});await until(()=>cli(['read',String(session)]).includes('mobile-'));
  const accounts=await phone.rpc({action:'accounts'});assert(accounts.accounts.some(a=>a.id==='mobile-test-account'));const switched=await phone.rpc({action:'switch_account',provider:'claude',account_id:'mobile-test-account'});assert(switched.accounts.some(a=>a.id==='mobile-test-account'&&a.active));assert(fs.readFileSync(path.join(dir,'active-accounts.env'),'utf8').includes(profile));
  await phone.rpc({action:'gate_reply',id:gateId,decision:'Approve'});assert.equal(JSON.parse(fs.readFileSync(path.join(dir,'orchestration.json'))).gates.find(g=>g.id===gateId).status,'resolved');
  await phone.rpc({action:'question_reply',id:question.id,body:'Proceed from mobile'});assert(JSON.parse(fs.readFileSync(path.join(dir,'orchestration.json'))).messages.some(m=>m.reply_to===question.id&&m.body==='Proceed from mobile'));
  const diff=await phone.rpc({action:'git',session,path:'example.txt'});assert(diff.patch.includes('+after'));await assert.rejects(phone.rpc({action:'git',session,path:'../../secrets'}));
  const handoff=await phone.rpc({action:'handoff',session,agent:'codex'});assert(handoff.to_session);await until(()=>cli(['read',String(handoff.to_session)]).includes('MK HANDOFF'));
  const saved={device_id:phone.authenticated.device_id,credential:phone.authenticated.credential};phone.socket.close();await new Promise(r=>setTimeout(r,80));other=await connect(url,saved);const resumed=await other.rpc({action:'snapshot'});assert(resumed.sessions.some(s=>s.id===session));
  // Daemon-owned notifications reach all providers with no desktop process running.
  const posts=[];webhookServer=require('node:http').createServer((req,res)=>{let body='';req.on('data',b=>body+=b);req.on('end',()=>{posts.push({path:req.url,body});res.writeHead(204);res.end();});});await new Promise(r=>webhookServer.listen(0,'127.0.0.1',r));
  const webhookBase=`http://127.0.0.1:${webhookServer.address().port}`;
  fs.writeFileSync(path.join(dir,'settings.toml'),`notifications = false\n[[webhooks]]\nprovider = "ntfy"\nurl = "${webhookBase}/ntfy"\n[[webhooks]]\nprovider = "telegram"\nurl = "${webhookBase}/telegram"\nchat_id = "fixture"\n[[webhooks]]\nprovider = "discord"\nurl = "${webhookBase}/discord"\n`);
  for(const [file,event] of [['working','UserPromptSubmit'],['waiting','PermissionRequest'],['done','Stop']])fs.writeFileSync(path.join(dir,file+'.json'),JSON.stringify({hook_event_name:event}));
  const hookCommand=`while [ ! -f '${dir}/webhook-go' ]; do sleep 0.02; done; '${mk}' hook claude < '${dir}/working.json'; '${mk}' hook claude < '${dir}/waiting.json'; '${mk}' hook claude < '${dir}/done.json'; printf 'SECRET_TERMINAL_CONTENT\\n'; exec /bin/cat`;
  const notifiedSession=Number(cli(['new','--cwd',repo,'--','/bin/sh','-c',hookCommand]));fs.writeFileSync(path.join(dir,'webhook-go'),'');await until(()=>posts.length===6);
  for(const provider of ['ntfy','telegram','discord']){const pair=posts.filter(p=>p.path==='/'+provider);assert.equal(pair.length,2);for(const p of pair){assert(p.body.includes(base+'/?session='+notifiedSession));assert(!p.body.includes('SECRET'));assert(!p.body.includes('pair='));assert(!p.body.includes('key='));}assert(pair[0].body.includes('waiting for input'));assert(pair[1].body.includes('completed'));}
  console.log('Daemon webhook E2E passed: ntfy, Telegram, Discord, waiting/done, credential-free deep links and no app process');
  console.log('PWA E2E passed: non-secure pure JS E2EE, gzip assets, approval, IME text, photo 0600, account switch, gate reply, diff, mock handoff and credential reconnect');
 } finally {cleanup();}
})().catch(error=>{console.error(error.stack);process.exitCode=1;});
