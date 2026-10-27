// Local real-SQLite serving path for Rust -> collector end-to-end tests.
import {createServer} from 'node:http';
import {Readable} from 'node:stream';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import worker from './src/worker.js';
import {LocalD1} from './sqlite.mjs';
export function localLimiter() {
  const counts = new Map(); let currentMinute = -1;
  return {async limit({key}) {
    const minute = Math.floor(Date.now()/60000);
    if (minute !== currentMinute) { counts.clear(); currentMinute=minute; }
    if (!counts.has(key) && counts.size >= 1024) return {success:false};
    const count=(counts.get(key)||0)+1; counts.set(key,count);
    return {success:count<=120};
  }};
}
export function serve({port=8787, path=join(tmpdir(),'krill-telemetry-local.sqlite'), token=process.env.ADMIN_TOKEN} = {}) {
  const db = new LocalD1(path);
  const env={DB:db, ADMIN_TOKEN:token, INGEST_RATE_LIMITER:localLimiter()};
  const server=createServer(async (req,res)=>{
    try {
      const headers=new Headers();
      for (const [name,value] of Object.entries(req.headers)) if (value!==undefined) headers.set(name,Array.isArray(value)?value.join(','):value);
      headers.set('CF-Connecting-IP',req.socket.remoteAddress || 'loopback');
      const init={method:req.method,headers};
      if (!['GET','HEAD'].includes(req.method)) {init.body=Readable.toWeb(req);init.duplex='half';}
      const response=await worker.fetch(new Request(new URL(req.url,'http://127.0.0.1'),init),env);
      res.writeHead(response.status,Object.fromEntries(response.headers));
      res.end(Buffer.from(await response.arrayBuffer()));
    } catch {res.writeHead(500,{'content-type':'application/json'});res.end('{"error":"local server error"}');}
  });
  server.requestTimeout=15000; server.headersTimeout=10000;
  server.on('close',()=>db.close());
  server.listen(port,'127.0.0.1');
  return server;
}
if (process.argv[1] && import.meta.url===pathToFileURL(process.argv[1]).href) {
  const server=serve({port:Number(process.env.PORT || 8787),path:process.env.TELEMETRY_DB});
  server.on('listening',()=>console.log('Local telemetry: http://127.0.0.1:'+server.address().port+' (SQLite; no remote resources)'));
  const stop=()=>server.close(); process.once('SIGINT',stop); process.once('SIGTERM',stop);
}
