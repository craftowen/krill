-- All limits count newly committed events, not attempted HTTP requests.
CREATE TABLE limits (
  id INTEGER PRIMARY KEY CHECK(id = 1),
  daily_rows INTEGER NOT NULL CHECK(daily_rows >= 0),
  daily_bytes INTEGER NOT NULL CHECK(daily_bytes >= 0),
  storage_rows INTEGER NOT NULL CHECK(storage_rows >= 0),
  storage_bytes INTEGER NOT NULL CHECK(storage_bytes >= 0)
) STRICT;
INSERT INTO limits VALUES(1, 2000, 8388608, 60000, 134217728);
CREATE TABLE service_state (
  id INTEGER PRIMARY KEY CHECK(id = 1),
  schema_version INTEGER NOT NULL CHECK(schema_version = 1),
  stored_rows INTEGER NOT NULL CHECK(stored_rows >= 0),
  stored_bytes INTEGER NOT NULL CHECK(stored_bytes >= 0)
) STRICT;
INSERT INTO service_state VALUES(1, 1, 0, 0);
CREATE TABLE daily_usage (
  day INTEGER PRIMARY KEY,
  rows INTEGER NOT NULL CHECK(rows >= 0),
  bytes INTEGER NOT NULL CHECK(bytes >= 0)
) STRICT;
CREATE TABLE events (
  event_id TEXT PRIMARY KEY NOT NULL CHECK(length(event_id) = 32),
  received_at INTEGER NOT NULL CHECK(received_at >= 0),
  event_json TEXT NOT NULL CHECK(json_valid(event_json)),
  logical_bytes INTEGER NOT NULL CHECK(logical_bytes = length(CAST(event_json AS BLOB)) AND logical_bytes <= 4096)
) STRICT;
CREATE INDEX events_received ON events(received_at DESC, event_id DESC);

-- Conflicting retries abort the entire D1 batch, including earlier admissions.
CREATE TRIGGER events_conflict BEFORE INSERT ON events
WHEN EXISTS(SELECT 1 FROM events WHERE event_id = NEW.event_id AND event_json != NEW.event_json)
BEGIN
  SELECT RAISE(ABORT, 'event id conflict');
END;
-- Ignore quota-limited NEW IDs; existing identical IDs still receive duplicate ACKs.
CREATE TRIGGER events_quota BEFORE INSERT ON events
WHEN NOT EXISTS(SELECT 1 FROM events WHERE event_id = NEW.event_id)
BEGIN
  SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM limits WHERE id = 1)
    OR NOT EXISTS(SELECT 1 FROM service_state WHERE id = 1)
    THEN RAISE(ABORT, 'admission unconfigured') END;
  SELECT CASE WHEN s.stored_rows >= l.storage_rows OR s.stored_bytes + NEW.logical_bytes > l.storage_bytes
    OR coalesce((SELECT rows FROM daily_usage WHERE day = NEW.received_at / 86400), 0) >= l.daily_rows
    OR coalesce((SELECT bytes FROM daily_usage WHERE day = NEW.received_at / 86400), 0) + NEW.logical_bytes > l.daily_bytes
    THEN RAISE(IGNORE) END FROM service_state s JOIN limits l ON l.id = s.id WHERE s.id = 1;
END;
CREATE TRIGGER events_account AFTER INSERT ON events
BEGIN
  UPDATE service_state SET stored_rows = stored_rows + 1, stored_bytes = stored_bytes + NEW.logical_bytes WHERE id = 1;
  INSERT INTO daily_usage(day, rows, bytes) VALUES(NEW.received_at / 86400, 1, NEW.logical_bytes)
    ON CONFLICT(day) DO UPDATE SET rows = rows + 1, bytes = bytes + NEW.logical_bytes;
END;
CREATE TRIGGER events_release AFTER DELETE ON events
BEGIN
  UPDATE service_state SET stored_rows = stored_rows - 1, stored_bytes = stored_bytes - OLD.logical_bytes WHERE id = 1;
  -- Daily admission is cumulative; deleting rows never restores today's quota.
END;
