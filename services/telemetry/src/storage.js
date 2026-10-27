const INSERT = `INSERT INTO events(id,install_id,name,ts_unix,app_version,os,arch,received_at)
 VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(id) DO NOTHING`;
export async function ingest(db, events, now) {
  const result = await db.batch(events.map(e => db.prepare(INSERT).bind(
    e.id, e.install_id, e.name, e.ts_unix, e.app_version, e.os, e.arch, now)));
  if (result.some(r => !r.success)) throw new Error('batch failed');
}
export async function stats(db, now, days) {
  const today = new Date(now * 1000).toISOString().slice(0, 10);
  const start = new Date((Math.floor(now / 86400) - days + 1) * 86400000).toISOString().slice(0, 10);
  const queries = [
    db.prepare(`SELECT count(*) AS observed_installs, count(first_open_ts) AS first_open_installs FROM installations`),
    db.prepare(`SELECT count(*) AS app_opens FROM events WHERE name='app_opened' AND ts_unix <= ?`).bind(now),
    db.prepare(`SELECT
      count(DISTINCT CASE WHEN day = ? THEN install_id END) AS dau,
      count(DISTINCT CASE WHEN day >= date(?, '-6 days') THEN install_id END) AS wau,
      count(DISTINCT install_id) AS mau
      FROM daily_active WHERE day BETWEEN date(?, '-29 days') AND ?`).bind(today, today, today, today),
    db.prepare(`WITH RECURSIVE dates(day) AS (SELECT ? UNION ALL SELECT date(day, '+1 day') FROM dates WHERE day < ?)
      SELECT day,
      (SELECT count(*) FROM installations WHERE date(first_seen_ts,'unixepoch')=day) AS observed_installs,
      (SELECT count(*) FROM installations WHERE date(first_open_ts,'unixepoch')=day) AS first_open_installs,
      (SELECT count(*) FROM events WHERE name='app_opened' AND ts_unix >= unixepoch(day) AND ts_unix < unixepoch(day,'+1 day') AND ts_unix <= ?) AS app_opens,
      (SELECT count(*) FROM daily_active a WHERE a.day=dates.day) AS active_devices
      FROM dates`).bind(start, today, now),
    db.prepare(`SELECT app_version AS value, count(*) AS installations FROM installations GROUP BY app_version ORDER BY installations DESC, value`),
    db.prepare(`SELECT os AS value, count(*) AS installations FROM installations GROUP BY os ORDER BY installations DESC, value`),
    ...[7, 30].map(n => db.prepare(`SELECT count(*) AS denominator,
      coalesce(sum(EXISTS(SELECT 1 FROM daily_active a WHERE a.install_id=i.install_id AND a.day=date(i.first_seen_ts,'unixepoch',?))),0) AS retained
      FROM installations i WHERE date(i.first_seen_ts,'unixepoch',?) < ?`).bind(`+${n} days`, `+${n} days`, today)),
  ];
  const result = await db.batch(queries);
  if (result.some(r => !r.success)) throw new Error('stats failed');
  const rows = result.map(r => r.results);
  return {
    as_of_unix: now, timezone: 'UTC', timeline_days: days,
    totals: {...rows[0][0], ...rows[1][0], ...rows[2][0]},
    timeline: rows[3], versions: rows[4], operating_systems: rows[5],
    retention: [7, 30].map((day, i) => ({day, ...rows[6+i][0],
      rate: rows[6+i][0].denominator ? rows[6+i][0].retained / rows[6+i][0].denominator : null,
      cohort: 'earliest observed event UTC date', maturity: 'target day completed before today UTC',
    })),
  };
}
