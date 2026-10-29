// Keep this closed wire schema aligned with krill-proto/error_reporting/types.rs.
export const MAX_BYTES = 65_536;
export const RETENTION = 30 * 86400;
export const HEX32 = /^[0-9a-f]{32}$(?![\s\S])/;
export const PROCESSES = ['krill-app', 'krilld', 'krill-cli', 'krill-holder'];
export const CODES = Object.freeze({
  session_start_failed: 'operation_error', daemon_listener_failed: 'operation_error',
  scheduler_failed: 'operation_error', recording_failed: 'operation_error',
  update_failed: 'operation_error', daemon_connect_failed: 'operation_error',
  panic: 'panic', ui_stall: 'ui_hang', native_crash: 'native_crash', unclean_exit: 'abnormal_exit',
});
export const KINDS = [...new Set(Object.values(CODES))];
const EXCEPTIONS = ['EXC_BAD_ACCESS', 'EXC_BAD_INSTRUCTION', 'EXC_ARITHMETIC',
  'EXC_BREAKPOINT', 'EXC_CRASH', 'EXC_RESOURCE', 'EXC_GUARD', 'UNKNOWN'];
const SIGNALS = ['SIGSEGV', 'SIGABRT', 'SIGBUS', 'SIGILL', 'SIGFPE', 'SIGTRAP', 'SIGKILL', 'UNKNOWN'];
const FIELDS = ['event_id', 'process', 'kind', 'error_code', 'release', 'build_id', 'os', 'arch',
  'source_loc', 'hang_bucket_ms', 'native'];
const OPTIONAL = ['source_loc', 'hang_bucket_ms', 'native'];
const bytes = value => new TextEncoder().encode(value).byteLength;
export class HttpError extends Error {
  constructor(status, message) { super(message); this.status = status; }
}
function closed(value, fields, optional = []) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).every(k => fields.includes(k))
    && fields.every(k => optional.includes(k) || Object.hasOwn(value, k));
}
export function validRelease(value) {
  return typeof value === 'string' && value.length <= 64
    && /^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$(?![\s\S])/.test(value);
}
function validSource(value) {
  if (typeof value !== 'string' || value.length > 200) return false;
  const parts = value.split(':');
  return (parts.length === 2 || parts.length === 3)
    && parts.slice(1).every(p => /^[1-9][0-9]{0,9}$(?![\s\S])/.test(p) && Number(p) <= 4294967295)
    && /^crates\/krill[a-z0-9-]*\/src\/(?:[A-Za-z0-9_-]+\/)*[A-Za-z0-9_-]+\.rs$(?![\s\S])/.test(parts[0]);
}
function validNative(value) {
  return closed(value, ['exception', 'signal', 'frames'])
    && EXCEPTIONS.includes(value.exception) && SIGNALS.includes(value.signal)
    && Array.isArray(value.frames) && value.frames.length <= 16
    && value.frames.every(f => closed(f, ['image_uuid', 'offset'])
      && typeof f.image_uuid === 'string' && HEX32.test(f.image_uuid)
      && Number.isSafeInteger(f.offset) && f.offset >= 0);
}
export function validate(value) {
  if (!closed(value, ['schema_version', 'events']) || value.schema_version !== 1
    || !Array.isArray(value.events) || value.events.length < 1 || value.events.length > 16)
    throw new HttpError(400, 'invalid batch');
  const seen = new Set();
  return value.events.map(e => {
    if (!closed(e, FIELDS, OPTIONAL) || typeof e.event_id !== 'string' || !HEX32.test(e.event_id)
      || seen.has(e.event_id) || !PROCESSES.includes(e.process)
      || typeof e.error_code !== 'string' || !Object.hasOwn(CODES, e.error_code) || CODES[e.error_code] !== e.kind
      || !validRelease(e.release) || typeof e.build_id !== 'string'
      || !/^[A-Za-z0-9.+-]{1,80}$(?![\s\S])/.test(e.build_id)
      || !['macos', 'windows', 'linux'].includes(e.os)
      || !['aarch64', 'x86_64', 'x86', 'arm', 'other'].includes(e.arch)
      || (e.source_loc != null && !validSource(e.source_loc))
      || (e.kind === 'ui_hang' ? ![500, 1500, 5000, 30000].includes(e.hang_bucket_ms) : e.hang_bucket_ms != null)
      || (e.kind === 'native_crash' ? !validNative(e.native) : e.native != null))
      throw new HttpError(400, 'invalid event');
    seen.add(e.event_id);
    // Rebuild in Rust serialization order; absent Option fields normalize to null.
    // Stable JSON makes ID collisions independent of whitespace/key order.
    const canonical = Object.fromEntries(FIELDS.map(k => [k, e[k] ?? null]));
    if (canonical.native) canonical.native = {
      exception: e.native.exception, signal: e.native.signal,
      frames: e.native.frames.map(f => ({image_uuid: f.image_uuid, offset: f.offset})),
    };
    const json = JSON.stringify(canonical);
    if (bytes(json) > 4096) throw new HttpError(400, 'event too large');
    return {event: canonical, json, bytes: bytes(json)};
  });
}
export async function readBody(request) {
  if (!/^application\/json(?:\s*;\s*charset=utf-8)?$(?![\s\S])/i.test(request.headers.get('content-type') || ''))
    throw new HttpError(415, 'application/json required');
  if (request.headers.has('content-encoding')) throw new HttpError(415, 'content encoding unsupported');
  const length = request.headers.get('content-length');
  if (length !== null && (!/^[0-9]+$(?![\s\S])/.test(length) || Number(length) > MAX_BYTES))
    throw new HttpError(413, 'body too large');
  if (!request.body) throw new HttpError(400, 'missing body');
  const reader = request.body.getReader();
  const chunks = []; let size = 0;
  try {
    while (true) {
      const {done, value} = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_BYTES) { await reader.cancel(); throw new HttpError(413, 'body too large'); }
      chunks.push(value);
    }
  } finally { reader.releaseLock(); }
  const data = new Uint8Array(size); let offset = 0;
  for (const chunk of chunks) { data.set(chunk, offset); offset += chunk.length; }
  try { return JSON.parse(new TextDecoder('utf-8', {fatal: true}).decode(data)); }
  catch { throw new HttpError(400, 'invalid JSON'); }
}
