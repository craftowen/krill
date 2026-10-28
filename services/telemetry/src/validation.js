export const MAX_BYTES = 65536;
export const MAX_AGE = 30 * 86400;
export const FUTURE_SKEW = 300;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
// Matches Cargo package versions, including bounded SemVer prerelease/build tags.
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const FIELDS = ['id', 'install_id', 'name', 'ts_unix', 'app_version', 'os', 'arch'];
export class HttpError extends Error {
  constructor(status, message) { super(message); this.status = status; }
}
function closed(value, fields) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).length === fields.length && fields.every(k => Object.hasOwn(value, k));
}
export function validate(value, now) {
  if (!closed(value, ['v', 'events']) || value.v !== 1 || !Array.isArray(value.events)
      || value.events.length < 1 || value.events.length > 50) throw new HttpError(400, 'invalid batch');
  for (const e of value.events) {
    if (!closed(e, FIELDS) || typeof e.id !== 'string' || !UUID.test(e.id)
      || typeof e.install_id !== 'string' || !UUID.test(e.install_id)
      || !['first_open', 'app_opened', 'active_day'].includes(e.name)
      || !Number.isSafeInteger(e.ts_unix) || e.ts_unix < now - MAX_AGE || e.ts_unix > now + FUTURE_SKEW
      || typeof e.app_version !== 'string' || e.app_version.length > 64 || !VERSION.test(e.app_version)
      || !['macos', 'windows', 'linux'].includes(e.os) || !['aarch64', 'x86_64'].includes(e.arch)) {
      throw new HttpError(400, 'invalid event');
    }
  }
  // Canonicalize UUID case so alternate casing cannot create extra installations.
  return value.events.map(e => ({...e, id: e.id.toLowerCase(), install_id: e.install_id.toLowerCase()}));
}
export async function readBody(request) {
  if (!/^application\/json(?:\s*;\s*charset=utf-8)?$/i.test(request.headers.get('content-type') || ''))
    throw new HttpError(415, 'application/json required');
  if (request.headers.has('content-encoding')) throw new HttpError(415, 'content encoding unsupported');
  const length = request.headers.get('content-length');
  if (length !== null && (!/^\d+$/.test(length) || Number(length) > MAX_BYTES)) throw new HttpError(413, 'body too large');
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
  const bytes = new Uint8Array(size); let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
  try { return JSON.parse(new TextDecoder('utf-8', {fatal: true}).decode(bytes)); }
  catch { throw new HttpError(400, 'invalid JSON'); }
}
