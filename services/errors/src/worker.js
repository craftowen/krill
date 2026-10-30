import {timingSafeEqual} from 'node:crypto';
import {CODES, HEX32, HttpError, KINDS, PROCESSES, readBody, validate, validRelease} from './validation.js';
import {cleanup, eventDetail, health, ingest, listEvents, stats} from './storage.js';
import {MOBILE_CODES, mobileAdmission, mobileCors, validateMobile} from './mobile.js';
const SECURITY = {
  'cache-control': 'no-store', 'x-content-type-options': 'nosniff', 'referrer-policy': 'no-referrer',
  'content-security-policy': "default-src 'none'; frame-ancestors 'none'",
};
function json(value, status = 200, headers = {}) {
  return Response.json(value, {status, headers: {...SECURITY, ...headers}});
}
async function authorized(request, secret) {
  if (typeof secret !== 'string' || secret.length < 32 || secret.length > 256)
    throw new HttpError(503, 'admin auth unconfigured');
  const header = request.headers.get('authorization') || '';
  if (!header.startsWith('Bearer ') || header.length > 263) return false;
  const digest = value => crypto.subtle.digest('SHA-256', new TextEncoder().encode(value));
  const [actual, expected] = await Promise.all([digest(header.slice(7)), digest(secret)]);
  return timingSafeEqual(new Uint8Array(actual), new Uint8Array(expected));
}
function parameters(url, allowed) {
  for (const key of url.searchParams.keys()) {
    if (!allowed.includes(key) || url.searchParams.getAll(key).length !== 1)
      throw new HttpError(400, 'invalid query');
  }
}
function integer(value, fallback, max) {
  if (value === null) return fallback;
  if (!/^[1-9][0-9]{0,2}$(?![\s\S])/.test(value) || Number(value) > max)
    throw new HttpError(400, 'invalid query');
  return Number(value);
}
function eventOptions(url) {
  parameters(url, ['limit', 'cursor', 'process', 'kind', 'error_code', 'release']);
  const options = {limit: integer(url.searchParams.get('limit'), 50, 100)};
  for (const key of ['process', 'kind', 'error_code', 'release']) {
    const value = url.searchParams.get(key);
    if (value === null) continue;
    const valid = key === 'process' ? [...PROCESSES, 'krill-mobile'].includes(value) : key === 'kind' ? KINDS.includes(value)
      : key === 'error_code' ? Object.hasOwn(CODES, value) || MOBILE_CODES.includes(value) : value === 'web' || validRelease(value);
    if (!valid) throw new HttpError(400, 'invalid query');
    options[key] = value;
  }
  const cursor = url.searchParams.get('cursor');
  if (cursor !== null) {
    const parts = cursor.split(':');
    if (parts.length !== 2 || !/^(0|[1-9][0-9]{0,15})$(?![\s\S])/.test(parts[0])
      || !Number.isSafeInteger(Number(parts[0])) || !HEX32.test(parts[1]))
      throw new HttpError(400, 'invalid cursor');
    options.cursor = {time: Number(parts[0]), id: parts[1]};
  }
  return options;
}
export async function handle(request, env, now = Math.floor(Date.now() / 1000)) {
  const url = new URL(request.url);
  const mobile = url.pathname === '/v1/mobile-events';
  if (url.pathname === '/v1/events' || mobile) {
    if (mobile) {
      const preflight = mobileAdmission(request);
      if (url.search) throw new HttpError(400, 'query parameters unsupported');
      if (preflight) return new Response(null, {status: 204, headers: {...SECURITY, ...preflight}});
    }
    if (request.method !== 'POST') return json({error: 'method not allowed'}, 405, {allow: 'POST'});
    if (url.search) throw new HttpError(400, 'query parameters unsupported');
    if (typeof env.INGEST_RATE_LIMITER?.limit !== 'function')
      throw new HttpError(503, 'rate limiter unconfigured');
    // Shared per-location admission key; no IP, device ID or request metadata stored.
    const rate = await env.INGEST_RATE_LIMITER.limit({key: 'v1/events'});
    if (rate?.success === false) return json({error: 'rate limited'}, 429, {'retry-after': '60'});
    if (rate?.success !== true) throw new HttpError(503, 'rate limiter unavailable');
    const events = (mobile ? validateMobile : validate)(await readBody(request));
    const ack = await ingest(env.DB, events, now);
    const count = ack.accepted_ids.length + ack.duplicate_ids.length;
    if (!count) return json({error: 'admission quota exceeded'}, 429, {'retry-after': '60'});
    return json(ack, 200, count < events.length ? {'retry-after': '60'} : {});
  }
  if (url.pathname === '/health') {
    if (request.method !== 'GET') return json({error: 'method not allowed'}, 405, {allow: 'GET'});
    if (url.search) throw new HttpError(400, 'query parameters unsupported');
    return json(await health(env.DB));
  }
  if (url.pathname === '/api/events' || url.pathname === '/api/stats' || url.pathname.startsWith('/api/events/')) {
    if (request.method !== 'GET') return json({error: 'method not allowed'}, 405, {allow: 'GET'});
    if (!await authorized(request, env.ADMIN_TOKEN))
      return json({error: 'unauthorized'}, 401, {'www-authenticate': 'Bearer'});
    if (url.pathname.startsWith('/api/events/')) {
      parameters(url, []);
      const id = url.pathname.slice('/api/events/'.length);
      if (!HEX32.test(id)) throw new HttpError(400, 'invalid event id');
      return json(await eventDetail(env.DB, now, id));
    }
    if (url.pathname === '/api/events') return json(await listEvents(env.DB, now, eventOptions(url)));
    parameters(url, ['days']);
    return json(await stats(env.DB, now, integer(url.searchParams.get('days'), 30, 30)));
  }
  return json({error: 'not found'}, 404);
}
export default {
  async fetch(request, env) {
    let response;
    try { response = await handle(request, env); }
    catch (error) {
      // Never log/reflect SQL, credentials, request headers, report bodies or paths.
      response = json({error: error instanceof HttpError ? error.message : 'service unavailable'},
        error instanceof HttpError ? error.status : 503);
    }
    if (new URL(request.url).pathname === '/v1/mobile-events') {
      for (const [key, value] of Object.entries(mobileCors(request))) response.headers.set(key, value);
    }
    return response;
  },
  async scheduled(controller, env) {
    await cleanup(env.DB, Math.floor(controller.scheduledTime / 1000));
  },
};
