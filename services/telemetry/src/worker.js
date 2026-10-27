import {timingSafeEqual} from 'node:crypto';
import {DASHBOARD, CSS, SCRIPT} from './dashboard.js';
import {HttpError, readBody, validate} from './validation.js';
import {ingest, stats} from './storage.js';
import {downloads} from './downloads.js';
const SECURITY = {
  'cache-control': 'no-store',
  'x-content-type-options': 'nosniff',
  'referrer-policy': 'no-referrer',
  'content-security-policy': "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
};
function json(value, status = 200, extra = {}) {
  return Response.json(value, {status, headers: {...SECURITY, ...extra}});
}
async function authorized(request, secret) {
  if (typeof secret !== 'string' || secret.length < 32 || secret.length > 256) throw new HttpError(503, 'admin auth unconfigured');
  const header = request.headers.get('authorization') || '';
  if (!header.startsWith('Bearer ') || header.length > 263) return false;
  const encode = value => crypto.subtle.digest('SHA-256', new TextEncoder().encode(value));
  const [actual, expected] = await Promise.all([encode(header.slice(7)), encode(secret)]);
  return timingSafeEqual(new Uint8Array(actual), new Uint8Array(expected));
}
export async function handle(request, env, now = Math.floor(Date.now()/1000)) {
  const url = new URL(request.url);
  if (url.pathname === '/v1/telemetry') {
    if (request.method !== 'POST') return json({error:'method not allowed'},405,{allow:'POST'});
    if (url.search) throw new HttpError(400, 'query parameters unsupported');
    // Rate-limit key only: never persist IP or a hash in D1 or application logs.
    if (!env.INGEST_RATE_LIMITER) throw new HttpError(503, 'rate limiter unconfigured');
    const hash = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(
      Math.floor(now/86400)+':'+(request.headers.get('CF-Connecting-IP') || 'local')));
    const key = Array.from(new Uint8Array(hash), b=>b.toString(16).padStart(2,'0')).join('');
    if (!(await env.INGEST_RATE_LIMITER.limit({key})).success) return json({error:'rate limited'},429,{'retry-after':'60'});
    const events = validate(await readBody(request), now);
    await ingest(env.DB, events, now);
    return json({accepted:events.length});
  }
  if (url.pathname === '/api/stats' || url.pathname === '/api/downloads') {
    if (request.method !== 'GET') return json({error:'method not allowed'},405,{allow:'GET'});
    if (!await authorized(request, env.ADMIN_TOKEN)) return json({error:'unauthorized'},401,{'www-authenticate':'Bearer'});
    if (url.pathname === '/api/downloads') {
      if (url.search) throw new HttpError(400, 'query parameters unsupported');
      return json(await downloads(env));
    }
    const days = url.searchParams.get('days') || '30';
    if (!/^\d{1,2}$/.test(days) || Number(days)<1 || Number(days)>90
      || [...url.searchParams.keys()].some(k=>k!=='days') || url.searchParams.getAll('days').length>1) throw new HttpError(400,'invalid timeline days');
    return json(await stats(env.DB, now, Number(days)));
  }
  if (['/', '/dashboard.css', '/dashboard.js'].includes(url.pathname)) {
    if (request.method !== 'GET') return json({error:'method not allowed'},405,{allow:'GET'});
    if (url.search) throw new HttpError(400,'query parameters unsupported');
    const [body, type] = url.pathname === '/' ? [DASHBOARD,'text/html'] : url.pathname.endsWith('.css') ? [CSS,'text/css'] : [SCRIPT,'text/javascript'];
    return new Response(body,{headers:{...SECURITY,'content-type':type+'; charset=utf-8'}});
  }
  return json({error:'not found'},404);
}
export default {
  async fetch(request, env) {
    try { return await handle(request, env); }
    catch (error) {
      // Do not reflect SQL, credentials, headers or client payloads in responses/logs.
      return json({error: error instanceof HttpError ? error.message : 'service unavailable'}, error instanceof HttpError ? error.status : 503);
    }
  },
};
