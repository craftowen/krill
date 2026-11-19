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
  try {
    await db.batch([db.prepare('INSERT INTO feedback(id, received_at, message, release, os, arch) VALUES(?, ?, ?, ?, ?, ?)')
      .bind(crypto.randomUUID(), now, feedback.message, feedback.release, feedback.os, feedback.arch)]);
  } catch (error) {
    if (String(error?.message).includes('feedback quota')) throw new HttpError(429, 'feedback quota exceeded');
    throw error;
  }
}
