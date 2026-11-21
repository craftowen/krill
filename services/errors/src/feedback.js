import {HttpError, validRelease} from './validation.js';
const FIELDS = ['message', 'release', 'os', 'arch'];
export const MAX_MESSAGE = 4000;
export function validateFeedback(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)
    || Object.keys(value).length !== FIELDS.length || !FIELDS.every(key => Object.hasOwn(value, key)))
    throw new HttpError(400, 'invalid feedback');
  const message = typeof value.message === 'string' ? value.message.trim() : '';
  if (!message || [...message].length > MAX_MESSAGE || !validRelease(value.release)
    || !['macos', 'windows', 'linux'].includes(value.os) || !['aarch64', 'x86_64'].includes(value.arch))
    throw new HttpError(400, 'invalid feedback');
  return {message, release: value.release, os: value.os, arch: value.arch};
}
export async function storeFeedback(db, feedback, now) {
  const id = crypto.randomUUID();
  try {
    await db.batch([db.prepare('INSERT INTO feedback(id, received_at, message, release, os, arch) VALUES(?, ?, ?, ?, ?, ?)')
      .bind(id, now, feedback.message, feedback.release, feedback.os, feedback.arch)]);
  } catch (error) {
    if (String(error?.message).includes('feedback quota')) throw new HttpError(429, 'feedback quota exceeded');
    throw error;
  }
  return id;
}
export async function notifyFeedback(webhookUrl, id, feedback, now, fetchImpl = fetch) {
  if (typeof webhookUrl !== 'string' || !webhookUrl) return;
  let url;
  try { url = new URL(webhookUrl); } catch { return; }
  if (url.protocol !== 'https:' && !(url.protocol === 'http:' && url.hostname === '127.0.0.1')) return;
  const body = JSON.stringify({
    allowed_mentions: {parse: []},
    embeds: [{
      color: 0xFEE75C,
      title: `크릴 피드백 접수 (${feedback.release}, ${feedback.os} ${feedback.arch})`,
      description: feedback.message,
      fields: [
        {name: '버전', value: feedback.release, inline: true},
        {name: '환경', value: `${feedback.os} (${feedback.arch})`, inline: true},
      ],
      footer: {text: `feedback ${id}`},
      timestamp: new Date(now * 1000).toISOString(),
    }],
  });
  for (const wait of [0, 2000]) {
    if (wait) await new Promise(resolve => setTimeout(resolve, wait));
    try {
      const response = await fetchImpl(webhookUrl, {
        method: 'POST',
        headers: {'content-type': 'application/json'},
        body,
        signal: AbortSignal.timeout(10000),
      });
      await response.body?.cancel?.();
      if (response.ok || (response.status < 500 && response.status !== 429)) return;
    } catch {}
  }
}
