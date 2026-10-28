import {HEX32, HttpError} from './validation.js';

export const MOBILE_CODES = ['mobile_connect_failed', 'mobile_discovery_failed', 'mobile_auth_failed',
  'mobile_rpc_failed', 'mobile_runtime_error', 'mobile_unhandled_rejection', 'mobile_asset_failed'];
const PHASES = ['idle', 'discovery', 'socket_open', 'challenge', 'authentication', 'ipc', 'snapshot', 'rpc', 'runtime', 'resource'];
const REASONS = ['websocket_error', 'websocket_closed', 'timeout', 'http_error', 'auth_required',
  'auth_rejected', 'protocol_error', 'network_offline', 'runtime_error', 'unhandled_rejection', 'resource_error', 'unknown'];
const SOURCES = ['index.html', 'app.js', 'client.js', 'diagnostics.js', 'crypto.js', 'storage.js', 'terminal.js',
  'updates.js', 'i18n.js', 'qr.js', 'sw.js', 'screens/home.js', 'screens/session.js', 'screens/chat.js',
  'screens/settings.js', 'screens/workspaces.js', 'screens/details.js'];
const FIELDS = ['event_id', 'process', 'kind', 'error_code', 'release', 'build_id', 'os', 'arch',
  'source_loc', 'hang_bucket_ms', 'native', 'diagnostics'];
const range = (min, max) => v => Number.isInteger(v) && v >= min && v <= max;
const enumOf = values => v => values.includes(v);
const hex = v => typeof v === 'string' && HEX32.test(v);
function closed(value, fields, required = fields) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).every(k => fields.includes(k)) && required.every(k => Object.hasOwn(value, k));
}
const CRUMB = {phase: enumOf(PHASES), reason: enumOf(REASONS), elapsed_ms: range(0, 300000),
  ws_code: range(1000, 4999), http_status: range(100, 599)};
const FRAME = {source_file: enumOf(SOURCES), line: range(0, 1000000), column: range(0, 1000000)};
function validFields(value, checks, required = []) {
  return closed(value, Object.keys(checks), required)
    && Object.entries(value).every(([key, item]) => checks[key](item));
}
const DIAGNOSTICS = {
  phase: enumOf(PHASES), route: enumOf(['relay', 'tunnel', 'tailscale', 'direct', 'unknown']),
  reason: enumOf(REASONS), attempt_id: hex, page_id: hex, elapsed_ms: range(0, 300000),
  retry_count: range(0, 10000), online: v => typeof v === 'boolean',
  visibility: enumOf(['visible', 'hidden', 'prerender', 'unknown']),
  browser: enumOf(['safari', 'chrome', 'firefox', 'edge', 'other']),
  ws_code: range(1000, 4999), http_status: range(100, 599),
  error_name: enumOf(['Error', 'TypeError', 'RangeError', 'ReferenceError', 'SyntaxError', 'URIError', 'EvalError', 'DOMException', 'Unknown']),
  source_file: enumOf(SOURCES), line: range(0, 1000000), column: range(0, 1000000),
  breadcrumbs: v => Array.isArray(v) && v.length <= 12
    && v.every(c => validFields(c, CRUMB, ['phase', 'reason', 'elapsed_ms'])),
  stack_frames: v => Array.isArray(v) && v.length <= 8
    && v.every(f => validFields(f, FRAME, Object.keys(FRAME))),
};
function canonicalFields(value, fields) {
  return Object.fromEntries(fields.filter(k => Object.hasOwn(value, k)).map(k => [k, value[k]]));
}
export function validateMobile(value) {
  if (!closed(value, ['schema_version', 'events']) || value.schema_version !== 1
    || !Array.isArray(value.events) || value.events.length < 1 || value.events.length > 16)
    throw new HttpError(400, 'invalid batch');
  const seen = new Set();
  return value.events.map(e => {
    if (!closed(e, FIELDS) || !hex(e.event_id) || seen.has(e.event_id)
      || e.process !== 'krill-mobile' || e.kind !== 'operation_error' || !MOBILE_CODES.includes(e.error_code)
      || e.release !== 'web' || typeof e.build_id !== 'string'
      || !/^(?:[0-9a-f]{16}|unversioned)$(?![\s\S])/.test(e.build_id)
      || !['ios', 'android', 'macos', 'windows', 'linux', 'other'].includes(e.os) || e.arch !== 'other'
      || e.source_loc !== null || e.hang_bucket_ms !== null || e.native !== null
      || !validFields(e.diagnostics, DIAGNOSTICS)) throw new HttpError(400, 'invalid event');
    seen.add(e.event_id);
    const event = canonicalFields(e, FIELDS);
    event.diagnostics = canonicalFields(e.diagnostics, Object.keys(DIAGNOSTICS));
    if (event.diagnostics.breadcrumbs) event.diagnostics.breadcrumbs = event.diagnostics.breadcrumbs
      .map(c => canonicalFields(c, Object.keys(CRUMB)));
    if (event.diagnostics.stack_frames) event.diagnostics.stack_frames = event.diagnostics.stack_frames
      .map(f => canonicalFields(f, Object.keys(FRAME)));
    const json = JSON.stringify(event), bytes = new TextEncoder().encode(json).byteLength;
    if (bytes > 4096) throw new HttpError(400, 'event too large');
    return {event, json, bytes};
  });
}

// Only canonical browser origins may be reflected. Never persist origin or other request metadata.
function allowedOrigin(origin) {
  if (origin === 'https://krill-mobile.pages.dev') return true;
  try {
    const url = new URL(origin);
    if (!['http:', 'https:'].includes(url.protocol) || url.origin !== origin) return false;
    if (url.protocol === 'https:' && url.port === ''
      && /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.trycloudflare\.com$/.test(url.hostname)) return true;
    if (url.hostname === 'localhost' || url.hostname === '[::1]') return true;
    const parts = url.hostname.split('.');
    if (parts.length !== 4 || parts.some(p => !/^(0|[1-9][0-9]{0,2})$/.test(p) || Number(p) > 255)) return false;
    return parts[0] === '127' || (parts[0] === '100' && Number(parts[1]) >= 64 && Number(parts[1]) <= 127);
  } catch { return false; }
}
export function mobileCors(request) {
  const origin = request.headers.get('origin');
  return {vary: 'Origin', ...(allowedOrigin(origin) ? {
    'access-control-allow-origin': origin, 'access-control-expose-headers': 'Retry-After',
  } : {})};
}
export function mobileAdmission(request) {
  const origin = request.headers.get('origin');
  if (origin !== null && !allowedOrigin(origin)) throw new HttpError(403, 'origin not allowed');
  if (request.headers.has('cookie') || request.headers.has('authorization'))
    throw new HttpError(400, 'credentials unsupported');
  if (request.method !== 'OPTIONS') return null;
  if (request.headers.get('access-control-request-method') !== 'POST')
    throw new HttpError(405, 'method not allowed');
  const headers = request.headers.get('access-control-request-headers');
  if (headers !== null && headers.split(',').some(h => h.trim().toLowerCase() !== 'content-type'))
    throw new HttpError(400, 'headers unsupported');
  return {'access-control-allow-methods': 'POST', 'access-control-allow-headers': 'Content-Type',
    'access-control-max-age': '600'};
}
