// Run against an explicitly selected loopback collector, never production.
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
const url=new URL(process.env.COLLECTOR_URL || 'http://127.0.0.1:8787');
if (url.protocol!=='http:' || !['localhost','127.0.0.1','[::1]'].includes(url.hostname) || url.username || url.password || url.search || url.hash) throw new Error('COLLECTOR_URL must be loopback HTTP without credentials/query/fragment');
if (!process.env.ADMIN_TOKEN) throw new Error('ADMIN_TOKEN required');
const install_id=randomUUID();const now=Math.floor(Date.now()/1000);const version='0.0.0-e2e.'+randomUUID();
const events=['first_open','app_opened','active_day'].map(name=>({id:randomUUID(),install_id,name,ts_unix:now,app_version:version,os:'macos',arch:'aarch64'}));
const post=events=>fetch(new URL('/v1/telemetry',url),{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({v:1,events})});
for (let i=0;i<2;i++) {const r=await post(events);assert.equal(r.status,200);assert.deepEqual(await r.json(),{accepted:3});}
const conflicted=await post([{...events[0],id:randomUUID(),install_id:randomUUID(),app_version:version+'-rollback'},{...events[1],app_version:'9.9.9'}]);assert.equal(conflicted.status,503);
assert.equal((await fetch(new URL('/api/stats',url))).status,401);
const r=await fetch(new URL('/api/stats?days=7',url),{headers:{authorization:'Bearer '+process.env.ADMIN_TOKEN}});assert.equal(r.status,200);const stats=await r.json();
assert.equal(stats.versions.find(v=>v.value===version).installations,1);
assert.equal(stats.versions.some(v=>v.value===version+'-rollback'),false);
assert.equal(stats.timeline.length,7);assert.ok(stats.totals.dau>=1);assert.ok(stats.totals.first_open_installs>=1);
console.log(JSON.stringify({result:'HTTP collector E2E passed',install_id,version,totals:stats.totals},null,2));
