-- In-app feedback: free text the user typed and chose to send. No account, device or IP identifier.
CREATE TABLE feedback (
  id TEXT PRIMARY KEY NOT NULL CHECK(length(id) = 36),
  received_at INTEGER NOT NULL CHECK(received_at >= 0),
  message TEXT NOT NULL CHECK(length(message) BETWEEN 1 AND 4000),
  release TEXT NOT NULL CHECK(length(release) <= 64),
  os TEXT NOT NULL CHECK(os IN ('macos', 'windows', 'linux')),
  arch TEXT NOT NULL CHECK(arch IN ('aarch64', 'x86_64'))
) STRICT;
CREATE INDEX feedback_received ON feedback(received_at DESC);
-- Public ingestion cannot grow the table without bound: 300 per UTC day, 50,000 stored.
CREATE TRIGGER feedback_quota BEFORE INSERT ON feedback
WHEN (SELECT count(*) FROM feedback WHERE received_at >= NEW.received_at / 86400 * 86400) >= 300
  OR (SELECT count(*) FROM feedback) >= 50000
BEGIN
  SELECT RAISE(ABORT, 'feedback quota');
END;
