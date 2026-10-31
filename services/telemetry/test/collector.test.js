import {test} from 'node:test';
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {once} from 'node:events';
import worker, {handle} from '../src/worker.js';
import {ingest, stats} from '../src/storage.js';
import {validate, MAX_AGE, FUTURE_SKEW} from '../src/validation.js';
import {DASHBOARD, SCRIPT, CSS} from '../src/dashboard.js';
import {LocalD1} from '../sqlite.mjs';
import {serve} from '../local.mjs';
const NOW = Date.parse('2026-10-27T12:00:00Z')/1000;
const SECRET = 'test-only-32-character-admin-secret';
const event = (changes={}) => ({id:randomUUID(),install_id:randomUUID(),name:'active_day',ts_unix:NOW,app_version:'0.2.7',os:'macos',arch:'aarch64',...changes});
const request = (events,headers={}) => new Request('https://telemetry.krill.page/v1/telemetry',{method:'POST',headers:{'content-type':'application/json',...headers},body:JSON.stringify({v:1,events})});
function environment(t) {const DB=new LocalD1();t.after(()=>DB.close());return {DB,ADMIN_TOKEN:SECRET,INGEST_RATE_LIMITER:{async limit(){return {success:true};}}};}
const count = (db,table) => db.sqlite.prepare(`SELECT count(*) AS n FROM ${table}`).get().n;
test('exact Rust wire contract; retry ack counts submitted rows including duplicates',async t=>{
  const env=environment(t); const one=event({name:'first_open'}); const two=event({install_id:one.install_id});
  const first=await handle(request([one,two]),env,NOW);assert.equal(first.status,200);assert.deepEqual(await first.json(),{accepted:2});
  const retry=await handle(request([one,two,two]),env,NOW);assert.deepEqual(await retry.json(),{accepted:3});
  assert.equal(count(env.DB,'events'),2);assert.equal(count(env.DB,'installations'),1);assert.equal(count(env.DB,'daily_active'),1);
});
test('UUID casing canonicalization preserves idempotency',async t=>{const env=environment(t);const e=event();await handle(request([e]),env,NOW);await handle(request([{...e,id:e.id.toUpperCase(),install_id:e.install_id.toUpperCase()}]),env,NOW);assert.equal(count(env.DB,'events'),1);});
test('closed schema, bounded versions/platforms, UUIDv4, counts, timestamps',()=>{
  const e=event();assert.equal(validate({v:1,events:[e]},NOW).length,1);
  for(const edit of [{path:'/private'},{id:'x'},{install_id:'00000000-0000-0000-0000-000000000000'},{name:'other'},{ts_unix:NOW+.5},{ts_unix:NOW-MAX_AGE-1},{ts_unix:NOW+FUTURE_SKEW+1},{os:'darwin'},{arch:'arm64'},{app_version:'x'.repeat(65)},{app_version:'0.2.7\n'}]) assert.throws(()=>validate({v:1,events:[{...e,...edit}]},NOW));
  for(const batch of [{v:2,events:[e]},{v:1,events:[]},{v:1,events:Array(51).fill(e)},{v:1,events:[e],extra:true}]) assert.throws(()=>validate(batch,NOW));
  validate({v:1,events:[{...e,ts_unix:NOW-MAX_AGE},{...e,ts_unix:NOW+FUTURE_SKEW}]},NOW);
});
test('invalid batch stores nothing, streamed bytes capped without content-length',async t=>{
  const env=environment(t);const invalid=await worker.fetch(request([event(),event({name:'bad'})]),env);assert.equal(invalid.status,400);assert.equal(count(env.DB,'events'),0);
  const huge=new Request('http://localhost/v1/telemetry',{method:'POST',headers:{'content-type':'application/json'},body:' '.repeat(65537)});
  assert.equal((await worker.fetch(huge,env)).status,413);
  assert.equal((await worker.fetch(request([event()],{'content-length':'65537'}),env)).status,413);
  assert.equal((await worker.fetch(request([event()],{'content-type':'text/plain'}),env)).status,415);
  assert.equal((await worker.fetch(request([event()],{'content-encoding':'gzip'}),env)).status,415);
  assert.equal((await worker.fetch(new Request('http://localhost/v1/telemetry'),env)).status,405);
});
test('storage failure and differing payload for existing ID roll back entire batch',async t=>{
  const env=environment(t);const e=event();await ingest(env.DB,[e],NOW);
  await assert.rejects(ingest(env.DB,[event(),{...e,app_version:'0.2.8'}],NOW));assert.equal(count(env.DB,'events'),1);assert.equal(count(env.DB,'installations'),1);
  env.DB.sqlite.exec("CREATE TRIGGER fail_insert BEFORE INSERT ON events WHEN NEW.app_version='9.9.9' BEGIN SELECT RAISE(ABORT,'test failure'); END;");
  await assert.rejects(ingest(env.DB,[event(),event({app_version:'9.9.9'})],NOW));assert.equal(count(env.DB,'events'),1);assert.equal(count(env.DB,'daily_active'),1);
  const failed=await worker.fetch(request([event({app_version:'9.9.9',ts_unix:Math.floor(Date.now()/1000)})]),env);assert.equal(failed.status,503);assert.deepEqual(await failed.json(),{error:'service unavailable'});
});
test('rate limiting fail-closed and retry hint',async t=>{const env=environment(t);env.INGEST_RATE_LIMITER.limit=async()=>({success:false});const r=await handle(request([event()]),env,NOW);assert.equal(r.status,429);assert.equal(r.headers.get('retry-after'),'60');assert.equal(count(env.DB,'events'),0);delete env.INGEST_RATE_LIMITER;assert.equal((await worker.fetch(request([event()]),env)).status,503);});
test('stats authentication, no token URLs, method and parameter bounds',async t=>{
  const env=environment(t);const get=(path='/api/stats',token=SECRET,method='GET')=>worker.fetch(new Request('https://telemetry.krill.page'+path,{method,headers:token?{authorization:'Bearer '+token}:{}}),env);
  assert.equal((await get('/api/stats',null)).status,401);assert.equal((await get('/api/stats','bad')).status,401);assert.equal((await get('/api/stats?token='+SECRET,null)).status,401);
  for(const path of ['/api/stats?days=0','/api/stats?days=91','/api/stats?days=2&days=3','/api/stats?token=x','/api/stats?days=2%20OR%201']) assert.equal((await get(path)).status,400);
  assert.equal((await get('/api/stats',SECRET,'POST')).status,405);
  const ok=await get();assert.equal(ok.status,200);assert.equal(ok.headers.get('cache-control'),'no-store');assert.equal(ok.headers.get('access-control-allow-origin'),null);
  env.ADMIN_TOKEN='';assert.equal((await get()).status,503);
});
test('metrics: UTC timeline, latest properties, exact active windows and mature retention denominator',async t=>{
  const env=environment(t);const at=date=>Date.parse(date+'Z')/1000;const A=randomUUID(),B=randomUUID(),C=randomUUID(),D=randomUUID();
  const e=(install_id,name,time,extra={})=>event({install_id,name,ts_unix:at(time),...extra});
  // Direct storage seeds older than client offline window to exercise historical cohorts.
  await ingest(env.DB,[e(A,'first_open','2026-09-01T23:59:59'),e(A,'active_day','2026-09-08T00:00:00'),e(A,'active_day','2026-10-01T23:00:00'),e(A,'app_opened','2026-10-27T01:00:00',{app_version:'0.3.0'}),e(A,'active_day','2026-10-27T02:00:00',{app_version:'0.3.0'}),e(B,'app_opened','2026-09-10T00:00:00',{os:'windows',arch:'x86_64'}),e(C,'first_open','2026-10-20T23:59:59'),e(C,'active_day','2026-10-27T00:00:00'),e(D,'app_opened','2026-10-26T23:59:59')],NOW);
  // Out-of-order older metadata must not downgrade version, repeated active events don't inflate DAU.
  await ingest(env.DB,[e(A,'active_day','2026-10-21T00:00:00'),e(A,'active_day','2026-10-27T02:00:00',{app_version:'0.3.0'}),e(A,'agent_active_day','2026-10-27T02:00:00',{app_version:'0.3.0'}),e(A,'star_prompt_shown','2026-10-27T02:01:00',{app_version:'0.3.0'}),e(A,'star_prompt_starred','2026-10-27T02:02:00',{app_version:'0.3.0'})],NOW);
  const data=await stats(env.DB,NOW,30);assert.deepEqual({...data.totals},{observed_installs:4,first_open_installs:2,app_opens:3,dau:2,wau:2,mau:2});
  assert.deepEqual({...data.verified},{verified_active_installs:2,returning_installs:1,agent_active_installs:1});
  assert.deepEqual(data.verified_operating_systems.map(x=>({...x})),[{value:'macos',installations:2}]);
  assert.deepEqual(data.star_funnel,{shown:{events:1,installs:1},starred:{events:1,installs:1},later:{events:0,installs:0},dismissed:{events:0,installs:0}});
  assert.equal(data.timeline.length,30);const last=data.timeline.at(-1);assert.equal(last.day,'2026-10-27');assert.equal(last.active_devices,2);assert.equal(last.app_opens,1);assert.equal(data.timeline.at(-2).observed_installs,1);
  assert.equal(data.versions.find(x=>x.value==='0.3.0').installations,1);assert.equal(data.versions.reduce((a,b)=>a+b.installations,0),4);
  assert.deepEqual(data.retention.map(x=>[x.day,x.denominator,x.retained,x.rate]),[[7,2,1,.5],[30,2,1,.5]]);
  assert.equal(data.operating_systems.find(x=>x.value==='windows').installations,1);
});
test('UTC windows include exactly today plus preceding six/29 days; first opens do not count as active',async t=>{
  const env=environment(t);const midnight=Math.floor(NOW/86400)*86400;
  for(const age of [0,6,7,29,30]) await ingest(env.DB,[event({ts_unix:midnight-age*86400})],NOW);
  await ingest(env.DB,[event({name:'first_open',ts_unix:midnight})],NOW);
  const d=await stats(env.DB,NOW,1);assert.equal(d.totals.dau,1);assert.equal(d.totals.wau,2);assert.equal(d.totals.mau,4);assert.equal(d.timeline.length,1);
  const empty=new LocalD1();t.after(()=>empty.close());assert.equal((await stats(empty,NOW,1)).retention[0].rate,null);
});
test('dashboard serves functional same-origin assets with CSP and safe rendering',async t=>{
  const env=environment(t);
  for(const path of ['/','/dashboard.js','/dashboard.css']) {const r=await worker.fetch(new Request('http://localhost'+path),env);assert.equal(r.status,200);assert.match(r.headers.get('content-security-policy'),/frame-ancestors 'none'/);}
  assert.match(DASHBOARD,/type="password"/);assert.match(DASHBOARD,/Retained \/ eligible/);assert.match(CSS,/#0b0d13/);assert.match(SCRIPT,/Authorization:'Bearer '\+token/);assert.doesNotMatch(SCRIPT,/innerHTML|localStorage|sessionStorage|https:\/\//);
  assert.equal((await worker.fetch(new Request('http://localhost/?token=x'),env)).status,400);
});
test('real local HTTP server -> same collector -> durable SQLite, auth readback',async t=>{
  const server=serve({port:0,path:':memory:',token:SECRET});t.after(()=>new Promise(resolve=>server.close(resolve)));await once(server,'listening');const base='http://127.0.0.1:'+server.address().port;
  const e=event({ts_unix:Math.floor(Date.now()/1000)});const body=JSON.stringify({v:1,events:[e]});
  for(let i=0;i<2;i++){const r=await fetch(base+'/v1/telemetry',{method:'POST',headers:{'content-type':'application/json'},body});assert.deepEqual(await r.json(),{accepted:1});}
  const s=await fetch(base+'/api/stats',{headers:{authorization:'Bearer '+SECRET}});assert.equal(s.status,200);const d=await s.json();assert.equal(d.totals.observed_installs,1);assert.equal(d.totals.dau,1);
});
test('download API requires admin auth and cannot take token/repository URLs',async t=>{const env=environment(t);assert.equal((await worker.fetch(new Request('http://localhost/api/downloads'),env)).status,401);assert.equal((await worker.fetch(new Request('http://localhost/api/downloads?repo=x',{headers:{authorization:'Bearer '+SECRET}}),env)).status,400);});
test('dashboard has unique element IDs and one download control',()=>{const ids=[...DASHBOARD.matchAll(/\bid="([^"]+)"/g)].map(x=>x[1]);assert.equal(new Set(ids).size,ids.length);assert.equal(ids.filter(x=>x==='downloads').length,1);});
test('SQLite acknowledged events persist across database reopen',async()=>{
  const {mkdtemp,rm}=await import('node:fs/promises');const {tmpdir}=await import('node:os');const {join}=await import('node:path');const dir=await mkdtemp(join(tmpdir(),'krill-collector-'));const path=join(dir,'events.sqlite');let db=new LocalD1(path);
  try{const e=event();await ingest(db,[e],NOW);db.close();db=new LocalD1(path);assert.equal(count(db,'events'),1);await ingest(db,[e],NOW);assert.equal(count(db,'events'),1);}finally{db.close();await rm(dir,{recursive:true});}
});
test('maximum 50-event batch and exactly 64 KiB body are accepted',async t=>{
  const env=environment(t);const events=Array.from({length:50},()=>event());const body=JSON.stringify({v:1,events});
  const r=await handle(new Request('http://localhost/v1/telemetry',{method:'POST',headers:{'content-type':'application/json; charset=utf-8'},body:body+' '.repeat(65536-Buffer.byteLength(body))}),env,NOW);
  assert.equal(r.status,200);assert.deepEqual(await r.json(),{accepted:50});assert.equal(count(env.DB,'events'),50);
});
