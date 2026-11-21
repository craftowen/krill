import test from 'node:test';
import assert from 'node:assert/strict';
import worker from '../src/worker.js';
import {SQLiteD1} from './sqlite.js';
const body = (overrides = {}) => ({message: '  탭이 가끔 안 닫혀요  ', release: '0.3.8', os: 'macos', arch: 'aarch64', ...overrides});
const post = (env, value, headers = {'content-type': 'application/json'}, ctx) => worker.fetch(
  new Request('https://errors.test/v1/feedback', {method: 'POST', headers, body: JSON.stringify(value)}), env, ctx);
function environment(t) {
  const env = {DB: new SQLiteD1(), INGEST_RATE_LIMITER: {limit: async () => ({success: true})}};
  t.after(() => env.DB.close());
  return env;
}
const rows = env => env.DB.sqlite.prepare('SELECT message, release, os, arch FROM feedback').all().map(r => ({...r}));

test('feedback stores the trimmed message with only version and platform', async t => {
  const env = environment(t);
  const response = await post(env, body());
  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), {ok: true});
  assert.deepEqual(rows(env), [{message: '탭이 가끔 안 닫혀요', release: '0.3.8', os: 'macos', arch: 'aarch64'}]);
});

test('feedback rejects empty, oversized, unknown fields and wrong media type without writing', async t => {
  const env = environment(t);
  for (const value of [body({message: '   '}), body({message: '가'.repeat(4001)}), body({email: 'a@b.c'}),
    body({os: 'ios'}), body({release: 'dev'}), [body()], 'text']) {
    assert.equal((await post(env, value)).status, 400, JSON.stringify(value).slice(0, 40));
  }
  assert.equal((await post(env, body(), {'content-type': 'text/plain'})).status, 415);
  assert.equal((await worker.fetch(new Request('https://errors.test/v1/feedback'), env)).status, 405);
  assert.equal((await post(env, body({message: '가'.repeat(4000)}))).status, 200);
  assert.equal(rows(env).length, 1);
});

test('feedback daily quota and edge rate limit refuse with 429', async t => {
  const env = environment(t);
  for (let i = 0; i < 300; i++) assert.equal((await post(env, body({message: `m${i}`}))).status, 200);
  assert.equal((await post(env, body())).status, 429);
  env.INGEST_RATE_LIMITER = {limit: async () => ({success: false})};
  assert.equal((await post(env, body())).status, 429);
  assert.equal(rows(env).length, 300);
});

test('feedback forwards stored payload to Discord webhook via waitUntil without mentions', async t => {
  const env = environment(t);
  const calls = [];
  const pending = [];
  env.FEEDBACK_WEBHOOK_URL = 'https://notify.test/feedback';
  env.FEEDBACK_FETCH = async (url, init) => {
    calls.push({url, method: init.method, headers: init.headers, body: JSON.parse(init.body)});
    return new Response(null, {status: 204});
  };
  const ctx = {waitUntil(promise) { pending.push(promise); }};
  const response = await post(env, body({message: '  @everyone 창이 안 닫혀요  '}), undefined, ctx);
  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), {ok: true});
  assert.equal(pending.length, 1);
  await Promise.all(pending);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, 'https://notify.test/feedback');
  assert.equal(calls[0].method, 'POST');
  assert.deepEqual(calls[0].body.allowed_mentions, {parse: []});
  assert.equal(calls[0].body.embeds.length, 1);
  const embed = calls[0].body.embeds[0];
  assert.equal(embed.description, '@everyone 창이 안 닫혀요');
  assert.match(embed.title, /0\.3\.8/);
  assert.match(embed.title, /macos aarch64/);
  assert.deepEqual(embed.fields, [
    {name: '버전', value: '0.3.8', inline: true},
    {name: '환경', value: 'macos (aarch64)', inline: true},
  ]);
  const storedId = env.DB.sqlite.prepare('SELECT id FROM feedback').get().id;
  assert.equal(embed.footer.text, `feedback ${storedId}`);
});

test('feedback still succeeds when webhook fails and skips webhook on 400 or 429', async t => {
  const env = environment(t);
  let calls = 0;
  env.FEEDBACK_WEBHOOK_URL = 'https://notify.test/feedback';
  env.FEEDBACK_FETCH = async () => {
    calls += 1;
    throw new Error('network down');
  };
  const pending = [];
  const ctx = {waitUntil(promise) { pending.push(promise); }};
  assert.equal((await post(env, body({message: 'invalid'}), undefined, ctx)).status, 200);
  await Promise.all(pending);
  assert.equal(calls, 2);
  assert.equal(rows(env).length, 1);

  calls = 0;
  assert.equal((await post(env, body({message: '   '}), undefined, ctx)).status, 400);
  env.INGEST_RATE_LIMITER = {limit: async () => ({success: false})};
  assert.equal((await post(env, body(), undefined, ctx)).status, 429);
  assert.equal(calls, 0);
});
