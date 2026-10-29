import {HttpError, RETENTION} from './validation.js';
export const CLEANUP_ROWS = 500;
async function batch(db, statements) {
  const result = await db.batch(statements);
  if (result.length !== statements.length || result.some(r => r.success !== true))
    throw new Error('database batch failed');
  return result;
}
export async function ingest(db, events, now) {
  const statements = events.flatMap(e => [
    db.prepare(`INSERT INTO events(event_id, received_at, event_json, logical_bytes)
      VALUES(?, ?, ?, ?) ON CONFLICT(event_id) DO NOTHING RETURNING event_id`)
      .bind(e.event.event_id, now, e.json, e.bytes),
    db.prepare('SELECT event_json FROM events WHERE event_id = ?').bind(e.event.event_id),
  ]);
  let result;
  try { result = await batch(db, statements); }
  catch (error) {
    // The static trigger error is the only DB error exposed as a client error.
    // D1 batch rolls back all earlier event/counter writes on this failure.
    if (error instanceof Error && error.message.includes('event id conflict'))
      throw new HttpError(400, 'event id conflict');
    throw error;
  }
  const ack = {schema_version: 1, accepted_ids: [], duplicate_ids: []};
  for (let i = 0; i < events.length; i++) {
    const e = events[i];
    if (result[i * 2].results.length) ack.accepted_ids.push(e.event.event_id);
    else if (result[i * 2 + 1].results[0]?.event_json === e.json) ack.duplicate_ids.push(e.event.event_id);
    // No stored row means admission was quota-limited: leave this ID pending.
  }
  return ack;
}
export async function health(db) {
  const result = await batch(db, [db.prepare(`SELECT schema_version FROM service_state
    WHERE id = 1 AND EXISTS(SELECT 1 FROM limits WHERE id = 1)`)]);
  if (result[0].results[0]?.schema_version !== 1) throw new Error('schema unavailable');
  return {ok: true, schema_version: 1};
}
export async function listEvents(db, now, options) {
  const where = ['received_at > ?', 'received_at <= ?'];
  const values = [now - RETENTION, now];
  if (options.cursor) {
    where.push('(received_at < ? OR (received_at = ? AND event_id < ?))');
    values.push(options.cursor.time, options.cursor.time, options.cursor.id);
  }
  for (const key of ['process', 'kind', 'error_code', 'release']) {
    if (options[key]) { where.push(`json_extract(event_json, '$.${key}') = ?`); values.push(options[key]); }
  }
  const result = await batch(db, [db.prepare(`SELECT event_id, received_at, event_json FROM events
    WHERE ${where.join(' AND ')} ORDER BY received_at DESC, event_id DESC LIMIT ?`)
    .bind(...values, options.limit + 1)]);
  const rows = result[0].results;
  const page = rows.slice(0, options.limit);
  const last = page.at(-1);
  return {
    schema_version: 1,
    events: page.map(r => ({received_at: r.received_at, event: JSON.parse(r.event_json)})),
    next_cursor: rows.length > options.limit ? `${last.received_at}:${last.event_id}` : null,
  };
}
export async function eventDetail(db, now, id) {
  const result = await batch(db, [db.prepare(`SELECT received_at, event_json FROM events
    WHERE event_id = ? AND received_at > ? AND received_at <= ?`).bind(id, now - RETENTION, now)]);
  const row = result[0].results[0];
  if (!row) throw new HttpError(404, 'not found');
  return {schema_version: 1, received_at: row.received_at, event: JSON.parse(row.event_json)};
}
export async function stats(db, now, days) {
  const start = Math.max(now - RETENTION, (Math.floor(now / 86400) - days + 1) * 86400 - 1);
  const within = 'received_at > ? AND received_at <= ?';
  const queries = [
    db.prepare(`SELECT count(*) AS rows, coalesce(sum(logical_bytes), 0) AS logical_bytes
      FROM events WHERE ${within}`).bind(start, now),
    db.prepare(`SELECT date(received_at, 'unixepoch') AS day, count(*) AS rows FROM events
      WHERE ${within} GROUP BY day ORDER BY day`).bind(start, now),
    db.prepare(`SELECT json_extract(event_json, '$.kind') AS kind,
      json_extract(event_json, '$.error_code') AS error_code,
      json_extract(event_json, '$.process') AS process, count(*) AS rows FROM events
      WHERE ${within} GROUP BY kind, error_code, process ORDER BY rows DESC, kind, error_code, process`).bind(start, now),
    db.prepare(`SELECT json_extract(event_json, '$.release') AS release, count(*) AS rows FROM events
      WHERE ${within} GROUP BY release ORDER BY rows DESC, release LIMIT 20`).bind(start, now),
    db.prepare('SELECT stored_rows, stored_bytes FROM service_state WHERE id = 1'),
    db.prepare('SELECT rows, bytes FROM daily_usage WHERE day = ?').bind(Math.floor(now / 86400)),
    db.prepare('SELECT daily_rows, daily_bytes, storage_rows, storage_bytes FROM limits WHERE id = 1'),
  ];
  const result = await batch(db, queries);
  const rows = result.map(r => r.results);
  return {
    schema_version: 1, as_of_unix: now, timezone: 'UTC', days, retention_days: 30,
    totals: rows[0][0], timeline: rows[1], groups: rows[2], releases: rows[3],
    storage: rows[4][0], daily_usage: rows[5][0] ?? {rows: 0, bytes: 0}, limits: rows[6][0],
  };
}
export async function cleanup(db, now) {
  // Both deletion scans are indexed and strictly bounded per cron invocation.
  await batch(db, [
    db.prepare(`DELETE FROM events WHERE event_id IN
      (SELECT event_id FROM events WHERE received_at <= ? ORDER BY received_at, event_id LIMIT ?)`)
      .bind(now - RETENTION, CLEANUP_ROWS),
    db.prepare(`DELETE FROM daily_usage WHERE day IN
      (SELECT day FROM daily_usage WHERE day < ? ORDER BY day LIMIT 32)`)
      .bind(Math.floor((now - RETENTION) / 86400)),
  ]);
}
