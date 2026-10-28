# Error receiver

Cloudflare Worker + D1 receiver for Krill's native opt-in and mobile automatic technical error reports. This service does not change consent or collect raw logs. Usage telemetry is a separate service.

## Wire contract

`POST /v1/events` accepts `{"schema_version":1,"events":[...]}` as UTF-8 JSON. The closed schema matches `crates/krill-proto/src/error_reporting/types.rs`: at most 16 events, 4 KiB canonical JSON per event, 64 KiB request body including whitespace. Unknown fields, invalid code/kind combinations, arbitrary text, user paths, out-of-range numbers, duplicate IDs within a batch and unsupported encodings are rejected. Optional Rust fields may be absent or null; native frames are limited to 16. Source locations are restricted to the public `crates/krill*/src/**/*.rs:line[:column]` format.

A successful response contains **exactly** these keys, to remain compatible with deployed clients using `deny_unknown_fields`:

```json
{"schema_version":1,"accepted_ids":["00000000000000000000000000000001"],"duplicate_ids":[]}
```

An accepted ID names a committed new event. A duplicate ID names an identical, previously committed canonical event. Reordered JSON keys and omitted optional nulls do not cause collisions. Duplicate acknowledgement does not consume quota or extend retention. ID uniqueness survives Worker restarts because it is stored in D1, and lasts until retention cleanup removes the row.

Reusing an ID with different content returns HTTP 400 and rolls back the entire batch, including all event and quota writes. Invalid batches also return 400. Existing clients isolate these batches and discard rejected singletons. There is no `rejected_ids` response field. A failed transaction returns 503 without an ACK; retrying after an uncertain commit yields duplicate acknowledgement.

Quota-limited events remain unacknowledged. A partially admitted batch returns 200 with only committed/duplicate IDs and `Retry-After: 60`; a fully quota-limited batch returns 429. Edge throttling returns 429. Missing, malformed or failing rate bindings fail closed with 503. The binding uses one shared route key per Cloudflare location, at 60 requests per minute; it processes no IP/device identifier. The edge limit is approximate and does not replace transactional quotas or account-level request limits.

## Mobile browser reports

`POST https://errors.krill.page/v1/mobile-events` accepts the same versioned batch envelope and returns the exact ACK above. The browser sends directly to this receiver even when it cannot reach the desktop. Mobile and native ingestion share the existing events table, ID namespace, edge admission key, transactional quotas, retention and cleanup. No new migration is required. The native `/v1/events` schema and its absence of CORS remain unchanged; each endpoint rejects the other endpoint's event schema.

Mobile technical error reporting is separate from usage telemetry and runs automatically unless a saved opt-out, Do Not Track or Global Privacy Control disables it. The browser persists only its reporting preference, using tiny values with backward-compatible legacy choices; a full/blocked origin that cannot reserve this preference fails closed. It never persists reports or identifiers. The queue is bounded to 32 events, 30 minutes and four attempts, with Retry-After kept in memory; reload/close discards unsent reports, and legacy disk outboxes are deleted. Disabling aborts the diagnostic request and clears the queue; re-enabling never sends errors observed while off. There is no installation or page identifier. The [collection notice](https://krill.page/privacy#mobile-errors) is linked in Settings and About. Native reporting remains opt-in. This public receiver validates report contents; it cannot independently verify the browser setting.

Every mobile event requires exactly these fields (all three native-only fields must be explicit `null`):

```json
{
  "event_id":"00000000000000000000000000000001",
  "process":"krill-mobile",
  "kind":"operation_error",
  "error_code":"mobile_connect_failed",
  "release":"web",
  "build_id":"0123456789abcdef",
  "os":"ios",
  "arch":"other",
  "source_loc":null,
  "hang_bucket_ms":null,
  "native":null,
  "diagnostics":{
    "phase":"socket_open",
    "route":"relay",
    "reason":"websocket_closed",
    "attempt_id":"00000000000000000000000000000002",
    "elapsed_ms":1200,
    "ws_code":1006,
    "breadcrumbs":[{"phase":"discovery","reason":"http_error","elapsed_ms":200,"http_status":502}]
  }
}
```

`event_id` and `attempt_id` are 32 lowercase hexadecimal characters. Event IDs support deduplication; the random connection-attempt ID correlates failures within a connection attempt. Neither is persisted by the browser. The automatic client omits `page_id`; the receiver validates and discards that optional legacy field before storage for compatibility. `build_id` is the PWA's 16-character lowercase hexadecimal asset hash or `unversioned`. `os` is `ios`, `android`, `macos`, `windows`, `linux` or `other`. Error codes are `mobile_connect_failed`, `mobile_discovery_failed`, `mobile_auth_failed`, `mobile_rpc_failed`, `mobile_runtime_error`, `mobile_unhandled_rejection` and `mobile_asset_failed`; kind is always `operation_error`.

`diagnostics` is a required closed object; every field inside it is optional, and present values must satisfy this table (null is not a value):

| Field | Allowed value |
| --- | --- |
| `phase` | `idle`, `discovery`, `socket_open`, `challenge`, `authentication`, `ipc`, `snapshot`, `rpc`, `runtime`, `resource` |
| `route` | `relay`, `tunnel`, `tailscale`, `direct`, `unknown` |
| `reason` | `websocket_error`, `websocket_closed`, `timeout`, `http_error`, `auth_required`, `auth_rejected`, `protocol_error`, `network_offline`, `runtime_error`, `unhandled_rejection`, `resource_error`, `unknown` |
| `attempt_id`, legacy `page_id` | Lowercase hex32; automatic client never sends `page_id`, and the receiver discards legacy page IDs before storage |
| `elapsed_ms` | Integer 0–300000 |
| `retry_count` | Integer 0–10000 |
| `online` | Boolean |
| `visibility` | `visible`, `hidden`, `prerender`, `unknown` |
| `browser` | `safari`, `chrome`, `firefox`, `edge`, `other` |
| `ws_code` | Integer 1000–4999, only when observed |
| `http_status` | Integer 100–599, only when observed |
| `error_name` | `Error`, `TypeError`, `RangeError`, `ReferenceError`, `SyntaxError`, `URIError`, `EvalError`, `DOMException`, `Unknown` |
| `source_file` | `index.html`, `app.js`, `client.js`, `diagnostics.js`, `crypto.js`, `storage.js`, `terminal.js`, `updates.js`, `i18n.js`, `qr.js`, `sw.js`, `screens/home.js`, `screens/session.js`, `screens/chat.js`, `screens/settings.js`, `screens/workspaces.js`, `screens/details.js` |
| `line`, `column` | Integer 0–1000000 |
| `breadcrumbs` | At most 12 closed objects, each requiring `phase`, `reason`, `elapsed_ms`, optionally `ws_code`, `http_status`, with the same bounds/enums |
| `stack_frames` | At most 8 closed objects, each requiring `source_file`, `line`, `column`, with the same allowlist/bounds; no raw stack or URLs |

Unknown/nested free-text fields are rejected, including raw URLs, referrers, user agents, account/device identifiers, relay IDs, authentication tokens, credentials, QR contents, terminal text, error messages and raw stacks. Allowed source filenames never include a URL, query, fragment or local path. Request headers and origins are not stored. Limits remain 16 events per batch, 4 KiB canonical JSON per event and 64 KiB streamed request body. Canonical ordering includes diagnostics and breadcrumb fields so reordered keys deduplicate correctly.

Only `/v1/mobile-events` supports CORS. Allowed origins are the exact `https://krill-mobile.pages.dev` origin and canonical HTTP(S) origins for `localhost`, IPv4 loopback `127.0.0.0/8`, IPv6 loopback `[::1]`, and Tailscale IPv4 `100.64.0.0/10`, including nondefault ports. Quick tunnel origins matching `https://<label>.trycloudflare.com` are also allowed, where the single label consists of lowercase letters, digits and interior hyphens; these require HTTPS and no explicit port. Private LAN addresses, other arbitrary domains (including Pages previews and Tailscale DNS names), `null`, URL credentials and noncanonical spellings are rejected rather than reflected. No wildcard or credentialed CORS is provided. Browser requests must use `credentials: 'omit'`; cookies and Authorization headers are rejected on this path. No-Origin non-browser requests can use mobile ingestion but receive no allow-origin header.

`OPTIONS` preflight allows `POST` with `Content-Type` only and a 600-second preflight lifetime. All responses on the mobile path, including validation, method, size, content-type, admission and internal failures, carry `Vary: Origin`; allowed origins also receive `Access-Control-Allow-Origin` and expose `Retry-After`. Untrusted origins receive 403 without reflection. Native ingestion, health and administrator APIs receive no CORS headers. The PWA host and desktop-hosted `/m/*` pages must permit `https://errors.krill.page` in their `connect-src` Content Security Policy before browser reports can reach the receiver.

## Admission and retention

The migration installs these policies in the singleton `limits` row:

| Limit | Policy |
| --- | --- |
| New events per UTC day | 2,000 |
| Canonical event JSON bytes per UTC day | 8 MiB |
| Stored event rows, including expired rows pending cleanup | 60,000 |
| Stored canonical event JSON bytes | 128 MiB |
| Query retention after receipt | 30 days |

These are admission policies, not measured performance or physical database size. Logical bytes count the UTF-8 canonical JSON payload only; indexes, counters, row metadata, SQLite pages and hosting backups have additional storage overhead. Daily rows/bytes are incremented only after insertion, and deletion never restores daily quota. Deletion releases stored row/byte counters. SQLite triggers perform these checks and updates inside the same D1 batch transaction as insertion and acknowledgement reads, with no asynchronous counter reservation or cache.

Queries hide events at or before the 30-day cutoff even before physical deletion. The `*/15 * * * *` cron deletes at most 500 expired events and 32 old daily counter rows per invocation, using indexed scans. Delayed cron execution can leave a backlog; storage caps still count those rows and may temporarily refuse admission. Retention applies to the live database; Cloudflare backup/recovery retention is managed separately by the hosting account.

## Operations API

`GET /health` is public and returns `{"ok":true,"schema_version":1}` only when the migrated database and admission configuration are available. Database failure returns 503.

`GET /api/events` and `GET /api/stats` require `Authorization: Bearer <ADMIN_TOKEN>`. The server-only secret must be 32–256 characters and is compared using fixed-length digests and constant-time comparison. Missing configuration returns 503, and missing/wrong credentials return 401. Responses use `Cache-Control: no-store` and have no cross-origin access headers.

- `/api/events?limit=50`: limit 1–100, default 50. Returns `{schema_version, events:[{received_at,event}], next_cursor}` in descending receipt-time/ID order. Pass the opaque `next_cursor` as `cursor` for the next page. Optional exact filters: `process`, `kind`, `error_code`, `release`. Unknown/repeated query parameters are rejected.
- Filter values include the native values plus `process=krill-mobile`, the seven mobile error codes and `release=web`. For example, `/api/events?process=krill-mobile&release=web&error_code=mobile_connect_failed` returns full sanitized diagnostics, including timing, route and breadcrumbs when reported.
- `/api/events/<event_id>`: exact lowercase hex32 ID, no query parameters. Returns `{schema_version,received_at,event}` with full sanitized diagnostics. Authentication is required before lookup; missing, expired and future-dated events return 404, malformed IDs/queries return 400. Details use the same retention boundary as list queries.
- `/api/stats?days=30`: days 1–30, default 30 UTC calendar days including today. Returns `{schema_version,as_of_unix,timezone,days,retention_days,totals,timeline,groups,releases,storage,daily_usage,limits}`. Totals count admitted reports, grouped by kind/code/process, with the top 20 releases. Storage includes expired rows pending deletion. These counts do not estimate distinct users, terminal sessions lost, or every occurrence suppressed by the client.

The Worker does not log report bodies, headers, IPs, SQL errors or credentials. Wrangler disables request observability and Logpush. Hosting infrastructure can still process connection metadata under its own configuration. Public ingestion can exhaust admission quotas; account request/cost limits remain an operator responsibility.

## Provision and verify

The config binds `DB` to `krill-errors`, `INGEST_RATE_LIMITER` to namespace `1002`, and routes `errors.krill.page` as a custom domain. The checked-in database ID is a placeholder; supply production account/database identifiers in a separate operator configuration outside the public source tree. Provisioning, domain activation, database migration and publication are separate operator actions; adding the source alone does not deploy the receiver.

From this directory, use the installed Wrangler CLI:

```sh
wrangler d1 migrations apply krill-errors --remote
wrangler secret put ADMIN_TOKEN
wrangler deploy --dry-run
wrangler deploy
```

Enter the administrator credential only through the secret prompt or a protected secret-input file. Never put it in the desktop app, repository, command arguments, report, or logs.

Node 22.13+ provides the real SQLite implementation; no package dependencies are required:

```sh
npm test
npm run test:http
```

The SQLite suite covers transaction rollback, lost commit replies, on-disk reopen/deduplication, strict native/mobile schema/privacy validation, streaming limits, shared daily/storage row and byte quotas, authentication, pagination, detail lookup, expiry, bounded cleanup, exact-origin preflight and CORS on errors. `test:http` starts an isolated local Wrangler/workerd/D1 instance, applies the migration, exercises native and mobile HTTP contracts (including preflight, diagnostics persistence, duplicate/collision behavior, filters/detail lookup, privacy and error CORS) and cron, and removes its temporary database. It never contacts the deployed receiver. Wrangler must be on PATH.

If an installed workerd predates the configured compatibility date, upgrade Wrangler or explicitly set `ERRORS_LOCAL_COMPATIBILITY_DATE=2026-09-28 npm run test:http` for local contract checks. This override is local to the test process and does not alter the deployment config; record the tested date separately from production verification. `ERRORS_WRANGLER` can select another installed Wrangler executable.
