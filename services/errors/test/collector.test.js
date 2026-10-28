import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import worker, {handle} from '../src/worker.js';
import {cleanup, CLEANUP_ROWS, ingest, listEvents} from '../src/storage.js';
import {CODES, HttpError, MAX_BYTES, RETENTION, validate} from '../src/validation.js';
import {SQLiteD1} from './sqlite.js';
const NOW = Math.floor(Date.now() / 1000);
const TOKEN = 'local-test-only-not-a-production-token';
const id = n => n.toString(16).padStart(32, '0');
const event = (n = 1, overrides = {}) => ({
  event_id: id(n), process: 'krill-app', kind: 'operation_error', error_code: 'session_start_failed',
  release: '0.2.16', build_id: 'eb1aa4b03f7a', os: 'macos', arch: 'aarch64',
  source_loc: 'crates/krill-app/src/main.rs:10:2', hang_bucket_ms: null, native: null, ...overrides,
});
function environment(t) {
  const env = {DB: new SQLiteD1(), ADMIN_TOKEN: TOKEN,
    INGEST_RATE_LIMITER: {limit: async () => ({success: true})}};
  t.after(() => env.DB.close());
  return env;
}
function request(events, extra = {}) {
  return new Request('https://errors.test/v1/events', {method: 'POST',
    headers: {'content-type': 'application/json'}, body: JSON.stringify({schema_version: 1, events}), ...extra});
}
const send = (env, events, extra) => worker.fetch(request(events, extra), env);
const get = (env, path, token = TOKEN) => worker.fetch(new Request(`https://errors.test${path}`,
  {headers: token ? {authorization: `Bearer ${token}`} : {}}), env);
const count = env => env.DB.sqlite.prepare('SELECT count(*) AS n FROM events').get().n;
const state = env => ({...env.DB.sqlite.prepare('SELECT stored_rows, stored_bytes FROM service_state').get()});
const usage = env => ({...env.DB.sqlite.prepare('SELECT rows, bytes FROM daily_usage').get()});

test('committed IDs use the exact existing Rust ACK keys and stable canonical JSON', async t => {
  const env = environment(t), e = event();
  const accepted = await send(env, [e]);
  assert.equal(accepted.status, 200);
  assert.deepEqual(await accepted.json(), {schema_version: 1, accepted_ids: [id(1)], duplicate_ids: []});
  const reordered = Object.fromEntries(Object.entries(e).reverse());
  const duplicate = await send(env, [reordered]);
  assert.deepEqual(await duplicate.json(), {schema_version: 1, accepted_ids: [], duplicate_ids: [id(1)]});
  const canonical = validate({schema_version: 1, events: [e]})[0];
  assert.equal(count(env), 1);
  assert.deepEqual(state(env), {stored_rows: 1, stored_bytes: canonical.bytes});
  assert.deepEqual(usage(env), {rows: 1, bytes: canonical.bytes});
  const omittedOptions = event();
  delete omittedOptions.hang_bucket_ms; delete omittedOptions.native;
  assert.equal((await send(env, [omittedOptions])).status, 200);
  assert.equal(count(env), 1);
});

test('duplicate acknowledgement survives closing and reopening the actual SQLite database', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'krill-errors-'));
  const path = join(directory, 'events.sqlite');
  let db = new SQLiteD1(path);
  try {
    await ingest(db, validate({schema_version: 1, events: [event()]}), NOW);
    db.close(); db = new SQLiteD1(path);
    const ack = await ingest(db, validate({schema_version: 1, events: [event()]}), NOW + 1);
    assert.deepEqual(ack, {schema_version: 1, accepted_ids: [], duplicate_ids: [id(1)]});
    assert.equal(db.sqlite.prepare('SELECT rows FROM daily_usage').get().rows, 1);
  } finally { db.close(); rmSync(directory, {recursive: true, force: true}); }
});

test('conflicting ID rolls back mixed batch and counters, and singleton conflict is 400', async t => {
  const env = environment(t);
  await send(env, [event()]);
  const before = state(env), beforeDaily = usage(env);
  const response = await send(env, [event(2), event(1, {release: '0.2.17'})]);
  assert.equal(response.status, 400);
  assert.deepEqual(await response.json(), {error: 'event id conflict'});
  assert.equal(count(env), 1); assert.deepEqual(state(env), before); assert.deepEqual(usage(env), beforeDaily);
  assert.equal((await send(env, [event(1, {build_id: 'other-build'})])).status, 400);
  assert.equal((await send(env, [event(2)])).status, 200);
});

test('database failure rolls back writes, hides SQL, and does not ACK', async t => {
  const env = environment(t), db = env.DB;
  const broken = {...env, DB: {batch: statements => db.batch([...statements,
    db.prepare('SELECT secret_password FROM nonexistent_private_table')]), prepare: sql => db.prepare(sql)}};
  const response = await send(broken, [event()]);
  assert.equal(response.status, 503);
  assert.deepEqual(await response.json(), {error: 'service unavailable'});
  assert.equal(count(env), 0); assert.deepEqual(state(env), {stored_rows: 0, stored_bytes: 0});
  assert.equal((await send(env, [event()])).status, 200);
});

test('lost commit response safely retries as durable duplicate', async t => {
  const env = environment(t), db = env.DB;
  const lostResponse = {...env, DB: {prepare: sql => db.prepare(sql), batch: async statements => {
    await db.batch(statements); throw new Error('simulated connection failure after commit');
  }}};
  assert.equal((await send(lostResponse, [event()])).status, 503);
  assert.deepEqual(await (await send(env, [event()])).json(),
    {schema_version: 1, accepted_ids: [], duplicate_ids: [id(1)]});
  assert.equal(count(env), 1); assert.equal(usage(env).rows, 1);
});

for (const dimension of ['daily_rows', 'daily_bytes', 'storage_rows', 'storage_bytes']) {
  test(`${dimension} enforces actual admission; partial ACK, duplicates and next attempt are correct`, async t => {
    const env = environment(t);
    const bytes = validate({schema_version: 1, events: [event()]})[0].bytes;
    env.DB.sqlite.prepare(`UPDATE limits SET ${dimension} = ?`).run(dimension.endsWith('rows') ? 1 : bytes);
    const partial = await send(env, [event(), event(2)]);
    assert.equal(partial.status, 200); assert.equal(partial.headers.get('retry-after'), '60');
    assert.deepEqual(await partial.json(), {schema_version: 1, accepted_ids: [id(1)], duplicate_ids: []});
    const denied = await send(env, [event(2)]);
    assert.equal(denied.status, 429); assert.equal(denied.headers.get('retry-after'), '60');
    assert.equal((await send(env, [event()])).status, 200);
    assert.equal(count(env), 1); assert.deepEqual(state(env), {stored_rows: 1, stored_bytes: bytes});
    assert.deepEqual(usage(env), {rows: 1, bytes});
    assert.equal((await send(env, [event(1, {release: '0.2.17'})])).status, 400);
  });
}

test('UTC daily quota resets next day; deleting events releases storage but not daily admission', async t => {
  const env = environment(t);
  env.DB.sqlite.exec('UPDATE limits SET daily_rows=1, storage_rows=1');
  const records = n => validate({schema_version: 1, events: [event(n)]});
  assert.equal((await ingest(env.DB, records(1), 86400 * 10)).accepted_ids.length, 1);
  env.DB.sqlite.exec('DELETE FROM events');
  assert.deepEqual(state(env), {stored_rows: 0, stored_bytes: 0});
  assert.equal((await ingest(env.DB, records(2), 86400 * 10 + 100)).accepted_ids.length, 0);
  assert.equal((await ingest(env.DB, records(2), 86400 * 11)).accepted_ids.length, 1);
  assert.equal(count(env), 1);
});

test('concurrent requests sharing SQLite cannot over-admit or count duplicates twice', async t => {
  const env = environment(t);
  env.DB.sqlite.exec('UPDATE limits SET daily_rows=2');
  const responses = await Promise.all([send(env, [event()]), send(env, [event()]),
    send(env, [event(2)]), send(env, [event(3)])]);
  assert.deepEqual(responses.map(r => r.status), [200, 200, 200, 429]);
  assert.equal(count(env), 2); assert.equal(usage(env).rows, 2);
  assert.equal(state(env).stored_rows, 2);
});

test('admission fails closed when limiter missing, broken, false or malformed', async t => {
  const env = environment(t);
  for (const [limiter, status] of [
    [undefined, 503], [{limit: async () => { throw new Error('private detail'); }}, 503],
    [{limit: async () => ({success: false})}, 429], [{limit: async () => ({})}, 503],
  ]) assert.equal((await send({...env, INGEST_RATE_LIMITER: limiter}, [event()])).status, status);
  assert.equal(count(env), 0);
  env.INGEST_RATE_LIMITER.limit = async ({key}) => { assert.equal(key, 'v1/events'); return {success: true}; };
  assert.equal((await send(env, [event()], {headers: {'content-type': 'application/json',
    'cf-connecting-ip': '192.0.2.1', 'x-client-private': 'must never persist'}})).status, 200);
  const stored = JSON.stringify(env.DB.sqlite.prepare('SELECT * FROM events').all());
  assert.ok(!stored.includes('192.0.2.1') && !stored.includes('must never persist'));
});

test('closed batch, event and nested schemas reject private/free-text fields without persistence', async t => {
  const env = environment(t);
  for (const extra of ['message', 'path', 'prompt', 'env', 'hostname', 'install_id', '__proto__']) {
    const payload = event(); Object.defineProperty(payload, extra, {value: '/Users/private/secret', enumerable: true});
    assert.equal((await send(env, [payload])).status, 400, extra);
  }
  for (const body of [{schema_version: 2, events: [event()]}, {schema_version: 1, events: [event()], ip: 'private'},
    {v: 1, events: [event()]}, {schema_version: 1, events: []}, {schema_version: 1, events: [event(), event()]}]) {
    const response = await send(env, [], {body: JSON.stringify(body)});
    assert.equal(response.status, 400); assert.ok(!(await response.text()).includes('private'));
  }
  assert.equal(count(env), 0);
});

test('all Rust event-code/kind variants and architectures accepted with matching optional fields', async t => {
  const env = environment(t);
  let n = 1;
  for (const [error_code, kind] of Object.entries(CODES)) {
    const e = event(n++, {error_code, kind});
    if (kind === 'ui_hang') e.hang_bucket_ms = 1500;
    if (kind === 'native_crash') e.native = {exception: 'EXC_BAD_ACCESS', signal: 'SIGSEGV',
      frames: [{image_uuid: id(99), offset: Number.MAX_SAFE_INTEGER}]};
    assert.equal((await send(env, [e])).status, 200, kind);
  }
  for (const arch of ['aarch64', 'x86_64', 'x86', 'arm', 'other'])
    assert.equal((await send(env, [event(n++, {arch})])).status, 200);
});

test('invalid enum pairings, options, paths, integer bounds and string bounds are rejected', async t => {
  const env = environment(t);
  for (const change of [
    {event_id: id(1).toUpperCase() + 'a'}, {event_id: id(1) + '\n'}, {process: 'private'}, {os: 'darwin'}, {arch: 'riscv'},
    {kind: 'panic'}, {error_code: 'toString'}, {error_code: 'unknown'}, {error_code: ['session_start_failed']},
    {error_code: {text: 'private'}}, {release: '01.2.3'}, {release: '1.2.3+private'},
    {release: '1.2.3\n'}, {release: '1000000000.2.3'}, {release: '1.2.3-'}, {build_id: 'a'.repeat(81)}, {build_id: 'abc\n'},
    {build_id: '/Users/private'}, {source_loc: '/Users/private/x.rs:10'}, {source_loc: 'crates/krill/src/../x.rs:10'},
    {source_loc: 'crates/krill/src/.hidden.rs:10'}, {source_loc: 'crates/krill/src/a.rs:01'},
    {source_loc: 'crates/krill/src/a.rs:4294967296'}, {source_loc: 'crates/krill/src/a.rs:1\n'},
    {hang_bucket_ms: 500}, {kind: 'ui_hang', error_code: 'ui_stall'},
    {kind: 'ui_hang', error_code: 'ui_stall', hang_bucket_ms: 501},
    {native: {exception: 'UNKNOWN', signal: 'UNKNOWN', frames: []}},
    {kind: 'native_crash', error_code: 'native_crash'},
  ]) assert.equal((await send(env, [event(1, change)])).status, 400, JSON.stringify(change));
  const native = (overrides = {}) => ({exception: 'UNKNOWN', signal: 'UNKNOWN', frames: [], ...overrides});
  for (const value of [native({exception: 'private'}), native({signal: 'SIGTERM'}), native({message: 'private'}),
    native({frames: Array.from({length: 17}, () => ({image_uuid: id(1), offset: 0}))}),
    ...[-1, 1.5, Number.MAX_SAFE_INTEGER + 1, '100'].map(offset => native({frames: [{image_uuid: id(1), offset}]})),
    native({frames: [{image_uuid: id(1), offset: 0, symbol: 'private'}]}),
    native({frames: [{image_uuid: 'third-party-library', offset: 0}]}),
  ]) assert.equal((await send(env, [event(1, {kind: 'native_crash', error_code: 'native_crash', native: value})])).status, 400);
  assert.equal(count(env), 0);
});

test('batch bound 16 and streaming cap 65536 bytes include undeclared/lying lengths', async t => {
  const env = environment(t);
  assert.equal((await send(env, Array.from({length: 16}, (_, i) => event(i + 1)))).status, 200);
  assert.equal((await send(env, Array.from({length: 17}, (_, i) => event(i + 100)))).status, 400);
  const raw = JSON.stringify({schema_version: 1, events: [event(100)]});
  assert.equal((await send(env, [], {body: raw.padEnd(MAX_BYTES, ' ')})).status, 200);
  let canceled = false;
  const stream = new ReadableStream({start(controller) {
    controller.enqueue(new Uint8Array(MAX_BYTES)); controller.enqueue(new Uint8Array(1));
  }, cancel() { canceled = true; }});
  const response = await send(env, [], {body: stream, duplex: 'half',
    headers: {'content-type': 'application/json', 'content-length': '1'}});
  assert.equal(response.status, 413); assert.equal(canceled, true);
  for (const length of ['65537', '-1', 'abc'])
    assert.equal((await send(env, [event(101)], {headers: {'content-type': 'application/json', 'content-length': length}})).status, 413);
  assert.equal(count(env), 17);
});

test('media type, encoding, invalid UTF-8 and JSON reject before storage', async t => {
  const env = environment(t);
  for (const extra of [{headers: {'content-type': 'text/plain'}},
    {headers: {'content-type': 'application/json', 'content-encoding': 'gzip'}}])
    assert.equal((await send(env, [event()], extra)).status, 415);
  for (const body of ['{', new Uint8Array([0xff]), 'null', '[]', ''])
    assert.equal((await send(env, [], {body})).status, 400);
  assert.equal(count(env), 0);
});

test('administrator Bearer credential is required; missing configuration fails closed', async t => {
  const env = environment(t);
  for (const path of ['/api/events', '/api/stats']) {
    for (const token of [null, 'wrong', TOKEN + 'a']) {
      const response = await get(env, path, token);
      assert.equal(response.status, 401); assert.equal(response.headers.get('www-authenticate'), 'Bearer');
    }
    for (const secret of [undefined, 'too-short', 'a'.repeat(257)])
      assert.equal((await get({...env, ADMIN_TOKEN: secret}, path)).status, 503);
    assert.equal((await get(env, path)).status, 200);
  }
  assert.equal((await get(env, '/health', null)).status, 200);
});

test('event query uses stable receipt/ID pagination, allowlisted filters and bounded limits', async t => {
  const env = environment(t);
  await ingest(env.DB, validate({schema_version: 1, events: [event(), event(2), event(3, {release: '0.2.17'})]}), NOW);
  const first = await (await get(env, '/api/events?limit=2')).json();
  assert.deepEqual(first.events.map(r => r.event.event_id), [id(3), id(2)]);
  assert.equal(first.events[0].received_at, NOW);
  const second = await (await get(env, `/api/events?limit=2&cursor=${first.next_cursor}`)).json();
  assert.deepEqual(second.events.map(r => r.event.event_id), [id(1)]); assert.equal(second.next_cursor, null);
  const filtered = await (await get(env, '/api/events?release=0.2.17&process=krill-app&kind=operation_error&error_code=session_start_failed')).json();
  assert.deepEqual(filtered.events.map(r => r.event.event_id), [id(3)]);
  for (const query of ['limit=101', 'limit=0', 'limit=01', 'limit=1&limit=2', 'cursor=bad',
    `cursor=9007199254740992:${id(1)}`, 'kind=private', 'error_code=toString', 'release=01.2.3', 'private=secret'])
    assert.equal((await get(env, `/api/events?${query}`)).status, 400, query);
});

test('expired rows are hidden before bounded cron cleanup; stats count admitted reports', async t => {
  const env = environment(t);
  for (let n = 1; n <= CLEANUP_ROWS + 3; n += 16) {
    const events = Array.from({length: Math.min(16, CLEANUP_ROWS + 4 - n)}, (_, i) => event(n + i));
    await ingest(env.DB, validate({schema_version: 1, events}), NOW - RETENTION);
  }
  await ingest(env.DB, validate({schema_version: 1, events: [event(999)]}), NOW);
  assert.equal(count(env), CLEANUP_ROWS + 4);
  const page = await listEvents(env.DB, NOW, {limit: 100});
  assert.deepEqual(page.events.map(r => r.event.event_id), [id(999)]);
  const result = await (await get(env, '/api/stats?days=30')).json();
  assert.equal(result.totals.rows, 1); assert.equal(result.storage.stored_rows, CLEANUP_ROWS + 4);
  assert.equal(result.groups[0].error_code, 'session_start_failed');
  assert.equal(result.groups[0].rows, 1); assert.equal(result.daily_usage.rows, 1);
  assert.equal(result.retention_days, 30); assert.equal(result.timezone, 'UTC');
  await worker.scheduled({scheduledTime: NOW * 1000}, env);
  assert.equal(count(env), 4); assert.equal(state(env).stored_rows, 4);
  await cleanup(env.DB, NOW);
  assert.equal(count(env), 1); assert.equal(state(env).stored_rows, 1);
  assert.equal(env.DB.sqlite.prepare('SELECT rows FROM daily_usage ORDER BY day').get().rows, 503);
  for (const query of ['days=0', 'days=31', 'days=2&days=3', 'unknown=x'])
    assert.equal((await get(env, `/api/stats?${query}`)).status, 400);
});

test('health checks migrated schema; all routes use restrictive cache/security headers', async t => {
  const env = environment(t);
  assert.deepEqual(await (await get(env, '/health', null)).json(), {ok: true, schema_version: 1});
  for (const path of ['/health', '/api/events', '/api/stats', '/missing']) {
    const r = await get(env, path);
    assert.equal(r.headers.get('cache-control'), 'no-store');
    assert.equal(r.headers.get('x-content-type-options'), 'nosniff');
    assert.equal(r.headers.get('access-control-allow-origin'), null);
  }
  assert.equal((await get(env, '/missing')).status, 404);
  assert.equal((await get(env, '/v1/events')).status, 405);
  assert.equal((await worker.fetch(new Request('https://errors.test/health', {method: 'POST'}), env)).status, 405);
  assert.equal((await get(env, '/health?private=x')).status, 400);
  await assert.rejects(handle(new Request('https://errors.test/v1/events?x=1', {method: 'POST'}), env, NOW),
    e => e instanceof HttpError && e.status === 400);
  env.DB.sqlite.exec('DELETE FROM limits');
  assert.equal((await get(env, '/health')).status, 503);
  assert.equal((await send(env, [event()])).status, 503);
  assert.equal(count(env), 0);
});
