import test from 'node:test';
import assert from 'node:assert/strict';
import worker from '../src/worker.js';
import {MOBILE_CODES, validateMobile} from '../src/mobile.js';
import {RETENTION} from '../src/validation.js';
import {ingest} from '../src/storage.js';
import {SQLiteD1} from './sqlite.js';
const ORIGIN = 'https://krill-mobile.pages.dev';
const TOKEN = 'local-mobile-test-only-admin-token';
const id = n => n.toString(16).padStart(32, '0');
const event = (n = 1, change = {}) => ({event_id: id(n), process: 'krill-mobile', kind: 'operation_error',
  error_code: 'mobile_connect_failed', release: 'web', build_id: '0123456789abcdef', os: 'ios', arch: 'other',
  source_loc: null, hang_bucket_ms: null, native: null, diagnostics: {phase: 'socket_open', route: 'relay',
    reason: 'websocket_closed', attempt_id: id(21), elapsed_ms: 1000, retry_count: 1,
    online: true, visibility: 'visible', browser: 'safari', ws_code: 1006,
    stack_frames: [{source_file: 'client.js', line: 12, column: 4}],
    breadcrumbs: [{phase: 'discovery', reason: 'http_error', elapsed_ms: 100, http_status: 503}]}, ...change});
function environment(t) {
  const env = {DB: new SQLiteD1(), ADMIN_TOKEN: TOKEN, INGEST_RATE_LIMITER: {limit: async () => ({success: true})}};
  t.after(() => env.DB.close()); return env;
}
function request(events, options = {}) {
  const {path = '/v1/mobile-events', headers = {}, ...extra} = options;
  return new Request(`https://errors.test${path}`, {method: 'POST',
    headers: {origin: ORIGIN, 'content-type': 'application/json', ...headers},
    body: JSON.stringify({schema_version: 1, events}), ...extra});
}
const send = (env, events, options) => worker.fetch(request(events, options), env);
const rows = env => env.DB.sqlite.prepare('SELECT * FROM events').all();
const admin = (env, path) => worker.fetch(new Request(`https://errors.test${path}`,
  {headers: {authorization: `Bearer ${TOKEN}`, origin: ORIGIN}}), env);
function cors(response, origin = ORIGIN) {
  assert.equal(response.headers.get('access-control-allow-origin'), origin);
  assert.equal(response.headers.get('access-control-allow-credentials'), null);
  assert.equal(response.headers.get('vary'), 'Origin');
  assert.equal(response.headers.get('cache-control'), 'no-store');
}

test('mobile commits sanitized details, canonical duplicate, and exposes filterable admin results without CORS', async t => {
  const env = environment(t), e = event();
  let response = await send(env, [e]); cors(response);
  assert.deepEqual(await response.json(), {schema_version: 1, accepted_ids: [id(1)], duplicate_ids: []});
  const reordered = {...e, diagnostics: Object.fromEntries(Object.entries(e.diagnostics).reverse())};
  reordered.diagnostics.breadcrumbs = e.diagnostics.breadcrumbs.map(c => Object.fromEntries(Object.entries(c).reverse()));
  reordered.diagnostics.stack_frames = e.diagnostics.stack_frames.map(f => Object.fromEntries(Object.entries(f).reverse()));
  response = await send(env, [Object.fromEntries(Object.entries(reordered).reverse())]);
  assert.deepEqual(await response.json(), {schema_version: 1, accepted_ids: [], duplicate_ids: [id(1)]});
  assert.equal(rows(env).length, 1);
  const stored = rows(env)[0];
  assert.deepEqual(JSON.parse(stored.event_json), e);
  assert.equal(stored.logical_bytes, new TextEncoder().encode(stored.event_json).byteLength);
  response = await admin(env, '/api/events?process=krill-mobile&release=web&kind=operation_error&error_code=mobile_connect_failed');
  assert.equal(response.status, 200); assert.equal(response.headers.get('access-control-allow-origin'), null);
  assert.deepEqual((await response.json()).events[0].event, e);
  const stats = await (await admin(env, '/api/stats')).json();
  assert.deepEqual(stats.groups, [{kind: 'operation_error', error_code: e.error_code, process: 'krill-mobile', rows: 1}]);
  assert.equal(stats.totals.distinct_attempts, 1);
  assert.equal(stats.details[0].distinct_attempts, 1);
  assert.equal(stats.details[0].sample_detail, 'websocket_closed');
  assert.deepEqual(stats.releases, [{release: 'web', rows: 1}]);
});

test('legacy page identifiers are discarded before storage and duplicate acknowledgement', async t => {
  const env = environment(t), expected = event();
  const legacy = {...expected, diagnostics: {...expected.diagnostics, page_id: id(22)}};
  assert.equal((await send(env, [legacy])).status, 200);
  assert.deepEqual(JSON.parse(rows(env)[0].event_json), expected);
  const response = await send(env, [{...legacy, diagnostics: {...legacy.diagnostics, page_id: id(23)}}]);
  assert.deepEqual(await response.json(), {schema_version: 1, accepted_ids: [], duplicate_ids: [id(1)]});
  const detail = await admin(env, `/api/events/${id(1)}`);
  assert.deepEqual((await detail.json()).event, expected);
});

test('mobile privacy preference headers stop ingestion before storage', async t => {
  const env = environment(t);
  for (const headers of [{dnt: '1'}, {'sec-gpc': '1'}, {dnt: '1', 'sec-gpc': '1'}]) {
    const response = await send(env, [event()], {headers});
    cors(response);
    assert.equal(response.status, 403);
  }
  assert.equal(rows(env).length, 0);
  assert.equal((await send(env, [event()], {headers: {dnt: '0'}})).status, 200);
});

test('all mobile codes, OS and allowlisted diagnostics accept their exact boundary values', async t => {
  const env = environment(t); let n = 1;
  for (const error_code of MOBILE_CODES) assert.equal((await send(env, [event(n++, {error_code})])).status, 200);
  for (const os of ['ios', 'android', 'macos', 'windows', 'linux', 'other'])
    assert.equal((await send(env, [event(n++, {os, build_id: 'unversioned', diagnostics: {}})])).status, 200);
  for (const diagnostics of [
    {elapsed_ms: 0, retry_count: 0, ws_code: 1000, http_status: 100, line: 0, column: 0, online: false},
    {elapsed_ms: 300000, retry_count: 10000, ws_code: 4999, http_status: 599, line: 1000000, column: 1000000},
    {error_name: 'DOMException', source_file: 'screens/home.js', visibility: 'prerender', browser: 'other'},
    {breadcrumbs: Array.from({length: 12}, () => ({phase: 'idle', reason: 'unknown', elapsed_ms: 0}))},
  ]) assert.equal((await send(env, [event(n++, {diagnostics})])).status, 200);
});

test('mobile schemas reject private fields, arbitrary text, object/prototype payloads, and malformed bounds', async t => {
  const env = environment(t);
  const privateText = 'https://private.example/?token=secret';
  for (const field of ['message', 'stack', 'url', 'referrer', 'userAgent', 'account', 'device', 'rid', 'token', 'qr', 'terminal', '__proto__', 'constructor']) {
    const diagnostics = JSON.parse(JSON.stringify(event().diagnostics));
    Object.defineProperty(diagnostics, field, {value: privateText, enumerable: true});
    for (const e of [{...event(), [field]: privateText}, event(1, {diagnostics}),
      event(1, {diagnostics: {breadcrumbs: [{phase: 'idle', reason: 'unknown', elapsed_ms: 0, [field]: privateText}]}})]) {
      const response = await send(env, [e]); cors(response); assert.equal(response.status, 400, field);
      assert.equal((await response.text()).includes(privateText), false);
    }
  }
  const invalidDiagnostics = [null, [], privateText, {phase: privateText}, {route: 'private'}, {reason: 'Error: token'},
    {attempt_id: id(1) + '\n'}, {page_id: '123'}, {online: 1}, {visibility: 'private'}, {browser: 'user agent'},
    {error_name: 'CustomError'}, {source_file: 'https://krill-mobile.pages.dev/client.js'},
    {source_file: '../app.js'}, {source_file: 'app.js?secret'}, {source_file: 'app.js\n'},
    {elapsed_ms: -1}, {elapsed_ms: 300001}, {elapsed_ms: 1.5}, {retry_count: 10001}, {retry_count: '1'},
    {ws_code: 999}, {ws_code: 5000}, {http_status: 99}, {http_status: 600}, {line: -1}, {column: 1000001},
    {breadcrumbs: [{}]}, {breadcrumbs: [null]}, {breadcrumbs: [{phase: 'idle', reason: 'unknown', elapsed_ms: 0, ws_code: 0}]},
    {breadcrumbs: Array.from({length: 13}, () => ({phase: 'idle', reason: 'unknown', elapsed_ms: 0}))},
  ];
  for (const field of ['phase', 'route', 'reason', 'attempt_id', 'page_id', 'elapsed_ms', 'retry_count', 'online',
    'visibility', 'browser', 'ws_code', 'http_status', 'error_name', 'source_file', 'line', 'column', 'breadcrumbs', 'stack_frames'])
    invalidDiagnostics.push({[field]: {constructor: privateText}});
  for (const diagnostics of invalidDiagnostics) assert.equal((await send(env, [event(1, {diagnostics})])).status, 400, JSON.stringify(diagnostics));
  assert.equal(rows(env).length, 0);
});

test('native and mobile endpoints reject cross-schema/cross-field combinations before persistence', async t => {
  const env = environment(t);
  assert.equal((await send(env, [event()], {path: '/v1/events'})).status, 400);
  for (const change of [{process: 'krill-app'}, {kind: 'panic'}, {error_code: 'panic'}, {error_code: 'toString'},
    {release: '1.2.3'}, {build_id: '0123456789ABCDEF'}, {build_id: 'hash'}, {build_id: '0123456789abcdef\n'},
    {os: 'darwin'}, {arch: 'aarch64'}, {source_loc: 'crates/krill/src/main.rs:1'},
    {hang_bucket_ms: 500}, {native: {exception: 'UNKNOWN', signal: 'UNKNOWN', frames: []}}])
    assert.equal((await send(env, [event(1, change)])).status, 400, JSON.stringify(change));
  for (const field of Object.keys(event())) {
    const e = event(); delete e[field];
    assert.equal((await send(env, [e])).status, 400, `missing ${field}`);
  }
  const native = {...event(), process: 'krill-app', release: '1.2.3', error_code: 'session_start_failed', os: 'macos', arch: 'aarch64'};
  assert.equal((await send(env, [native], {path: '/v1/events'})).status, 400, 'native diagnostics rejected');
  delete native.diagnostics;
  assert.equal((await send(env, [native])).status, 400, 'native event rejected on mobile');
  const response = await send(env, [native], {path: '/v1/events'});
  assert.equal(response.status, 200); assert.equal(response.headers.get('access-control-allow-origin'), null);
});

test('mobile reuses transactional conflicts, shared native quotas, and 30-day query retention', async t => {
  const env = environment(t), e = event();
  await send(env, [e]);
  assert.equal((await send(env, [event(2), event(1, {diagnostics: {}})])).status, 400);
  assert.equal(rows(env).length, 1);
  env.DB.sqlite.exec('UPDATE limits SET daily_rows=1');
  let response = await send(env, [event(2)]); cors(response); assert.equal(response.status, 429);
  assert.equal(response.headers.get('retry-after'), '60');
  assert.equal(response.headers.get('access-control-expose-headers'), 'Retry-After');
  assert.equal((await send(env, [e])).status, 200);
  env.DB.sqlite.exec('UPDATE limits SET daily_rows=100');
  await ingest(env.DB, validateMobile({schema_version: 1, events: [event(3)]}), Math.floor(Date.now() / 1000) - RETENTION);
  const result = await (await admin(env, '/api/events?release=web')).json();
  assert.deepEqual(result.events.map(r => r.event.event_id), [id(1)]);
  await worker.scheduled({scheduledTime: Date.now()}, env);
  assert.equal(rows(env).length, 1);
});

test('mobile preflight admits only canonical PWA, loopback and Tailscale origins, never credentials', async t => {
  const env = environment(t);
  for (const origin of [ORIGIN, 'http://localhost:3000', 'https://localhost', 'http://127.0.0.1:9123',
    'http://127.9.8.7:8080', 'http://[::1]:9123', 'https://100.64.0.0:9123', 'http://100.127.255.255:9123',
    'https://a.trycloudflare.com', 'https://example-random-123.trycloudflare.com']) {
    const response = await worker.fetch(new Request('https://errors.test/v1/mobile-events', {method: 'OPTIONS', headers: {
      origin, 'access-control-request-method': 'POST', 'access-control-request-headers': 'Content-Type',
    }}), env);
    assert.equal(response.status, 204, origin); cors(response, origin);
    assert.equal(response.headers.get('access-control-allow-methods'), 'POST');
    assert.equal(response.headers.get('access-control-allow-headers'), 'Content-Type');
    assert.equal(await response.text(), '');
  }
  for (const origin of ['null', 'https://evil.test', 'https://krill-mobile.pages.dev.evil.test', ORIGIN + '/',
    'http://localhost.evil.test', 'http://100.63.255.255:9123', 'http://100.128.0.0:9123',
    'http://192.168.1.2:9123', 'https://user:secret@localhost:3000', 'http://127.1:3000', 'http://0x7f000001:3000',
    'ftp://localhost', 'https://krill-mobile.pages.dev:444', 'http://[::ffff:7f00:1]:9123',
    'http://example.trycloudflare.com', 'https://example.trycloudflare.com:444',
    'https://example.trycloudflare.com:443', 'https://sub.example.trycloudflare.com',
    'https://example.trycloudflare.com.evil.test', 'https://-bad.trycloudflare.com',
    'https://bad-.trycloudflare.com', 'https://bad_name.trycloudflare.com', 'https://secret@example.trycloudflare.com']) {
    const response = await send(env, [event()], {headers: {origin}});
    assert.equal(response.status, 403, origin); assert.equal(response.headers.get('access-control-allow-origin'), null);
    assert.equal((await response.text()).includes(origin), false);
  }
  for (const headers of [{cookie: 'private=secret'}, {authorization: 'Bearer secret'}]) {
    const response = await send(env, [event()], {headers}); cors(response); assert.equal(response.status, 400);
  }
  for (const method of ['GET', 'DELETE']) {
    const response = await worker.fetch(new Request('https://errors.test/v1/mobile-events', {method: 'OPTIONS', headers: {
      origin: ORIGIN, 'access-control-request-method': method,
    }}), env);
    cors(response); assert.equal(response.status, 405);
  }
  const response = await worker.fetch(new Request('https://errors.test/v1/mobile-events', {method: 'OPTIONS', headers: {
    origin: ORIGIN, 'access-control-request-method': 'POST', 'access-control-request-headers': 'content-type, authorization',
  }}), env);
  cors(response); assert.equal(response.status, 400); assert.equal(rows(env).length, 0);
});

test('every mobile error response keeps scoped CORS; native, admin and other paths never gain it', async t => {
  const env = environment(t);
  const scenarios = [
    [env, request([event()], {body: '{'}), 400],
    [env, request([event()], {path: '/v1/mobile-events?private=secret'}), 400],
    [env, new Request('https://errors.test/v1/mobile-events', {headers: {origin: ORIGIN}}), 405],
    [env, request([event()], {body: ' '.repeat(65537)}), 413],
    [env, request([event()], {headers: {'content-type': 'text/plain'}}), 415],
    [{...env, INGEST_RATE_LIMITER: {limit: async () => ({success: false})}}, request([event()]), 429],
    [{...env, INGEST_RATE_LIMITER: undefined}, request([event()]), 503],
    [{...env, DB: {prepare() { throw new Error('private db detail'); }}}, request([event()]), 503],
  ];
  for (const [config, req, status] of scenarios) {
    const response = await worker.fetch(req, config); cors(response); assert.equal(response.status, status);
    assert.equal((await response.text()).includes('secret'), false);
  }
  for (const path of ['/v1/events', '/api/events', '/api/stats', '/health', '/missing']) {
    const response = await worker.fetch(new Request(`https://errors.test${path}`, {method: 'OPTIONS', headers: {
      origin: ORIGIN, 'access-control-request-method': 'POST',
    }}), env);
    assert.equal(response.headers.get('access-control-allow-origin'), null, path);
  }
});

test('mobile bounds batches and body bytes before storage; client metadata never reaches SQLite', async t => {
  const env = environment(t);
  assert.equal((await send(env, Array.from({length: 17}, (_, i) => event(i + 1)))).status, 400);
  assert.equal((await send(env, [event(), event()])).status, 400);
  assert.equal((await send(env, [], {body: JSON.stringify({schema_version: 1, events: [event()], url: 'private'})})).status, 400);
  const e = event();
  const response = await send(env, [e], {headers: {'user-agent': 'private UA', referer: 'https://private.test/secret',
    'cf-connecting-ip': '192.0.2.1', 'x-private': 'private header'}});
  assert.equal(response.status, 200);
  const stored = JSON.stringify(rows(env));
  for (const forbidden of [ORIGIN, 'private', '192.0.2.1']) assert.equal(stored.includes(forbidden), false);
});

test('admin detail lookup requires auth, exact IDs and empty query; expired/future details stay hidden', async t => {
  const env = environment(t), now = Math.floor(Date.now() / 1000), e = event();
  await ingest(env.DB, validateMobile({schema_version: 1, events: [e]}), now);
  await ingest(env.DB, validateMobile({schema_version: 1, events: [event(2)]}), now - RETENTION);
  await ingest(env.DB, validateMobile({schema_version: 1, events: [event(3)]}), now + 86400);
  const response = await admin(env, `/api/events/${id(1)}`);
  assert.equal(response.status, 200); assert.equal(response.headers.get('access-control-allow-origin'), null);
  assert.deepEqual(await response.json(), {schema_version: 1, received_at: now, event: e});
  for (const n of [2, 3, 4]) assert.equal((await admin(env, `/api/events/${id(n)}`)).status, 404);
  for (const path of [`${id(1)}?private=secret`, 'bad', `${id(1)}/extra`, `${id(1)}%0a`, ''])
    assert.equal((await admin(env, `/api/events/${path}`)).status, 400);
  assert.equal((await worker.fetch(new Request(`https://errors.test/api/events/${id(1)}`), env)).status, 401);
});

for (const dimension of ['daily_rows', 'daily_bytes', 'storage_rows', 'storage_bytes']) {
  test(`mobile ${dimension} uses the native admission counter and preserves duplicate ACK under exhaustion`, async t => {
    const env = environment(t);
    const native = {...event(1), process: 'krill-app', release: '1.2.3', error_code: 'session_start_failed', os: 'macos'};
    delete native.diagnostics;
    assert.equal((await send(env, [native], {path: '/v1/events'})).status, 200);
    const mobile = event(2), bytes = validateMobile({schema_version: 1, events: [mobile]})[0].bytes;
    const stored = rows(env)[0].logical_bytes;
    env.DB.sqlite.prepare(`UPDATE limits SET ${dimension} = ?`).run(dimension.endsWith('rows') ? 2 : stored + bytes);
    const response = await send(env, [mobile, event(3)]); cors(response); assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), {schema_version: 1, accepted_ids: [id(2)], duplicate_ids: []});
    assert.equal((await send(env, [event(3)])).status, 429);
    assert.equal((await send(env, [mobile])).status, 200);
    assert.equal(rows(env).length, 2);
    const state = env.DB.sqlite.prepare('SELECT stored_rows, stored_bytes FROM service_state').get();
    assert.equal(state.stored_rows, 2); assert.equal(state.stored_bytes, stored + bytes);
  });
}

test('sanitized stack frames enforce closed public-file positions, bounded count and exact integer limits', async t => {
  const env = environment(t);
  const frame = {source_file: 'client.js', line: 0, column: 1000000};
  assert.equal((await send(env, [event(1, {diagnostics: {stack_frames: []}})])).status, 200);
  assert.equal((await send(env, [event(2, {diagnostics: {stack_frames: Array.from({length: 8}, () => frame)}})])).status, 200);
  const invalid = [null, {}, 'Error: secret', [null], [{}], Array.from({length: 9}, () => frame),
    [{...frame, source_file: 'https://private.test/client.js?token=secret'}], [{...frame, source_file: '/Users/private.js'}],
    [{...frame, line: -1}], [{...frame, column: 1000001}], [{...frame, line: 1.5}], [{...frame, column: '2'}],
    [{...frame, message: 'secret'}], [{...frame, stack: 'secret'}], [{source_file: 'app.js', line: 1}],
    [{...frame, source_file: {url: 'private'}}], JSON.parse('[{"source_file":"app.js","line":1,"column":2,"__proto__":"secret"}]')];
  for (const stack_frames of invalid) {
    const response = await send(env, [event(3, {diagnostics: {stack_frames}})]);
    assert.equal(response.status, 400, JSON.stringify(stack_frames)); cors(response);
  }
  assert.equal(rows(env).length, 2);
});
