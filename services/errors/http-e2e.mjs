// Isolated local workerd/D1 HTTP verification. Never contacts the deployed receiver.
import assert from 'node:assert/strict';
import {spawn, spawnSync} from 'node:child_process';
import {mkdtempSync, rmSync} from 'node:fs';
import {createServer} from 'node:net';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {setTimeout as delay} from 'node:timers/promises';
const cwd = fileURLToPath(new URL('.', import.meta.url));
const directory = mkdtempSync(join(tmpdir(), 'krill-errors-http-'));
const token = 'local-http-test-only-not-production';
const wrangler = process.env.ERRORS_WRANGLER || 'wrangler';
let child;
try {
  const migration = spawnSync(wrangler, ['d1', 'migrations', 'apply', 'krill-errors',
    '--local', '--persist-to', directory], {cwd, input: 'y\n', encoding: 'utf8', env: {...process.env, WRANGLER_SEND_METRICS: 'false'}});
  assert.equal(migration.status, 0, 'local D1 migration must succeed');
  const server = createServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  const base = `http://127.0.0.1:${port}`;
  const compat = process.env.ERRORS_LOCAL_COMPATIBILITY_DATE;
  child = spawn(wrangler, ['dev', '--local', '--ip', '127.0.0.1', '--port', String(port),
    '--persist-to', directory, '--test-scheduled', '--var', `ADMIN_TOKEN:${token}`,
    ...(compat ? ['--compatibility-date', compat] : [])],
    {cwd, stdio: 'ignore', env: {...process.env, WRANGLER_SEND_METRICS: 'false'}});
  let ready = false;
  for (let i = 0; i < 200; i++) {
    if (child.exitCode !== null) throw new Error('local Wrangler exited before health');
    try { if ((await fetch(`${base}/health`)).status === 200) { ready = true; break; } } catch {}
    await delay(100);
  }
  assert.ok(ready, 'local Worker health within 20 seconds');
  const e = {event_id: '00000000000000000000000000000001', process: 'krill-app',
    kind: 'operation_error', error_code: 'session_start_failed', release: '0.2.16', build_id: 'http-test',
    os: 'macos', arch: 'aarch64', source_loc: null, hang_bucket_ms: null, native: null};
  const post = events => fetch(`${base}/v1/events`, {method: 'POST', headers: {'content-type': 'application/json'},
    body: JSON.stringify({schema_version: 1, events})});
  let response = await post([e]);
  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), {schema_version: 1, accepted_ids: [e.event_id], duplicate_ids: []});
  response = await post([e]);
  assert.deepEqual(await response.json(), {schema_version: 1, accepted_ids: [], duplicate_ids: [e.event_id]});
  const second = {...e, event_id: '00000000000000000000000000000002'};
  assert.equal((await post([second, {...e, release: '0.2.17'}])).status, 400, 'D1 trigger must abort mixed collision batch');
  assert.equal((await fetch(`${base}/api/events`)).status, 401);
  const headers = {authorization: `Bearer ${token}`};
  response = await fetch(`${base}/api/events`, {headers});
  const events = await response.json();
  assert.equal(events.events.length, 1, 'collision batch must have rolled back second ID');
  assert.deepEqual(events.events[0].event, e);
  const stats = await (await fetch(`${base}/api/stats`, {headers})).json();
  assert.equal(stats.totals.rows, 1); assert.equal(stats.daily_usage.rows, 1); assert.equal(stats.storage.stored_rows, 1);
  assert.equal((await fetch(`${base}/__scheduled?cron=*/15+*+*+*+*`)).status, 200);
  assert.equal((await post([{...second, message: 'must reject private text'}])).status, 400);
  console.log(`PASS local workerd/D1 HTTP (compatibility date: ${compat || 'wrangler config'}): health, exact ACK, durable duplicate, collision rollback, admin auth/events/stats, cron, closed schema`);
} finally {
  if (child && child.exitCode === null) {
    const exited = new Promise(resolve => child.once('exit', resolve));
    child.kill('SIGTERM');
    const timeout = setTimeout(() => child.kill('SIGKILL'), 5000);
    await exited;
    clearTimeout(timeout);
  }
  rmSync(directory, {recursive: true, force: true});
}
