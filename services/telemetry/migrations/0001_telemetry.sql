-- Only the closed wire properties are stored. No IP, headers, paths or content.
CREATE TABLE events (
  id TEXT PRIMARY KEY,
  install_id TEXT NOT NULL,
  name TEXT NOT NULL CHECK (name IN ('first_open','app_opened','active_day','agent_active_day','star_prompt_shown','star_prompt_starred','star_prompt_later','star_prompt_dismissed')),
  ts_unix INTEGER NOT NULL,
  app_version TEXT NOT NULL,
  os TEXT NOT NULL CHECK (os IN ('macos','windows','linux')),
  arch TEXT NOT NULL CHECK (arch IN ('aarch64','x86_64')),
  received_at INTEGER NOT NULL
) STRICT;
CREATE INDEX events_day_name ON events(name, ts_unix);
CREATE TABLE installations (
  install_id TEXT PRIMARY KEY,
  first_seen_ts INTEGER NOT NULL,
  first_open_ts INTEGER,
  latest_ts INTEGER NOT NULL,
  latest_event_id TEXT NOT NULL,
  app_version TEXT NOT NULL,
  os TEXT NOT NULL,
  arch TEXT NOT NULL
) STRICT;
CREATE INDEX installations_first_seen ON installations(first_seen_ts);
CREATE TABLE daily_active (
  day TEXT NOT NULL,
  install_id TEXT NOT NULL,
  PRIMARY KEY(day, install_id)
) STRICT;
CREATE INDEX daily_active_install ON daily_active(install_id, day);
-- Duplicate IDs must be identical: a collision aborts the entire D1 batch.
CREATE TRIGGER event_id_conflict BEFORE INSERT ON events
WHEN EXISTS (SELECT 1 FROM events e WHERE e.id = NEW.id AND
 (e.install_id != NEW.install_id OR e.name != NEW.name OR e.ts_unix != NEW.ts_unix
 OR e.app_version != NEW.app_version OR e.os != NEW.os OR e.arch != NEW.arch))
BEGIN SELECT RAISE(ABORT, 'event id conflict'); END;
CREATE TRIGGER event_rollup AFTER INSERT ON events BEGIN
  INSERT INTO installations VALUES (
    NEW.install_id, NEW.ts_unix,
    CASE WHEN NEW.name='first_open' THEN NEW.ts_unix END,
    NEW.ts_unix, NEW.id, NEW.app_version, NEW.os, NEW.arch
  ) ON CONFLICT(install_id) DO UPDATE SET
    first_seen_ts = min(first_seen_ts, excluded.first_seen_ts),
    first_open_ts = CASE WHEN first_open_ts IS NULL THEN excluded.first_open_ts
      WHEN excluded.first_open_ts IS NULL THEN first_open_ts
      ELSE min(first_open_ts, excluded.first_open_ts) END,
    app_version = CASE WHEN (excluded.latest_ts, excluded.latest_event_id) > (latest_ts, latest_event_id) THEN excluded.app_version ELSE app_version END,
    os = CASE WHEN (excluded.latest_ts, excluded.latest_event_id) > (latest_ts, latest_event_id) THEN excluded.os ELSE os END,
    arch = CASE WHEN (excluded.latest_ts, excluded.latest_event_id) > (latest_ts, latest_event_id) THEN excluded.arch ELSE arch END,
    latest_event_id = CASE WHEN (excluded.latest_ts, excluded.latest_event_id) > (latest_ts, latest_event_id) THEN excluded.latest_event_id ELSE latest_event_id END,
    latest_ts = max(latest_ts, excluded.latest_ts);
  INSERT INTO daily_active(day, install_id)
    SELECT date(NEW.ts_unix, 'unixepoch'), NEW.install_id WHERE NEW.name='active_day'
    ON CONFLICT DO NOTHING;
END;
