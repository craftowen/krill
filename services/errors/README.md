# Error receiver

Cloudflare Worker + D1 receiver for Krill's existing, independently opt-in error reports. This service does not change consent or collect raw logs. Usage telemetry is a separate service.

## Wire contract

`POST /v1/events` accepts `{"schema_version":1,"events":[...]}` as UTF-8 JSON. The closed schema matches `crates/krill-proto/src/error_reporting/types.rs`: at most 16 events, 4 KiB canonical JSON per event, 64 KiB request body including whitespace. Unknown fields, invalid code/kind combinations, arbitrary text, user paths, out-of-range numbers, duplicate IDs within a batch and unsupported encodings are rejected. Optional Rust fields may be absent or null; native frames are limited to 16. Source locations are restricted to the public `crates/krill*/src/**/*.rs:line[:column]` format.

A successful response contains **exactly** these keys, to remain compatible with deployed clients using `deny_unknown_fields`:

```json
{"schema_version":1,"accepted_ids":["00000000000000000000000000000001"],"duplicate_ids":[]}
```

An accepted ID names a committed new event. A duplicate ID names an identical, previously committed canonical event. Reordered JSON keys and omitted optional nulls do not cause collisions. Duplicate acknowledgement does not consume quota or extend retention. ID uniqueness survives Worker restarts because it is stored in D1, and lasts until retention cleanup removes the row.

Reusing an ID with different content returns HTTP 400 and rolls back the entire batch, including all event and quota writes. Invalid batches also return 400. Existing clients isolate these batches and discard rejected singletons. There is no `rejected_ids` response field. A failed transaction returns 503 without an ACK; retrying after an uncertain commit yields duplicate acknowledgement.

Quota-limited events remain unacknowledged. A partially admitted batch returns 200 with only committed/duplicate IDs and `Retry-After: 60`; a fully quota-limited batch returns 429. Edge throttling returns 429. Missing, malformed or failing rate bindings fail closed with 503. The binding uses one shared route key per Cloudflare location, at 60 requests per minute; it processes no IP/device identifier. The edge limit is approximate and does not replace transactional quotas or account-level request limits.

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

The SQLite suite covers transaction rollback, lost commit replies, on-disk reopen/deduplication, strict schema/privacy validation, streaming limits, daily/storage row and byte quotas, authentication, pagination, expiry and bounded cleanup. `test:http` starts an isolated local Wrangler/workerd/D1 instance, applies the migration, exercises HTTP contracts and cron, and removes its temporary database. It never contacts the deployed receiver. Wrangler must be on PATH.

If an installed workerd predates the configured compatibility date, upgrade Wrangler or explicitly set `ERRORS_LOCAL_COMPATIBILITY_DATE=2026-09-28 npm run test:http` for local contract checks. This override is local to the test process and does not alter the deployment config; record the tested date separately from production verification. `ERRORS_WRANGLER` can select another installed Wrangler executable.
