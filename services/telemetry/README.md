# Krill telemetry collector

Cloudflare Worker + D1 collector and authenticated operations dashboard. **Not deployed:** the production database ID is an explicit placeholder and no production secret is included. `https://telemetry.krill.page/v1/telemetry` requires operator provisioning and deployment before it can collect events. See [desktop behavior and opt-out controls](CLIENT.md). Error reporting is a separate feature and is unchanged by this collector.

## Wire contract

`POST /v1/telemetry`, `Content-Type: application/json` (optional `charset=utf-8`), no content encoding or query parameters:

```json
{"v":1,"events":[{"id":"85c75a81-3d02-4354-9af2-1a59ba080cab","install_id":"ac055969-8235-4db6-bf05-20ace03f3472","name":"active_day","ts_unix":1793000000,"app_version":"0.2.7","os":"macos","arch":"aarch64"}]}
```

This is a strict closed schema: 1–50 events, at most 65,536 bytes including chunked bodies, UUIDv4 IDs, 64-character maximum version matching numeric `major.minor.patch` with optional bounded prerelease/build tags, OS `macos|windows|linux`, architecture `aarch64|x86_64`, integer seconds from `now - 2,592,000` through `now + 300` inclusive. Replace the example timestamp with current time when testing. Unknown properties/events and invalid batches are rejected without writes. UUID case is canonicalized.

A durable successful D1 transaction returns HTTP 200 `{"accepted":N}` with the submitted count, including identical duplicates. Retries must keep event IDs and payloads unchanged. A conflicting existing ID or database error rolls back the entire batch and returns generic 503 with no acknowledgement. Events, installation rollups, and daily-active records share the same transaction via SQLite triggers. No queues or post-response persistence are involved.

Status codes: 400 invalid schema/JSON/parameters, 401 admin auth rejected, 405 method, 413 oversized body, 415 media/encoding, 429 rate limit (`Retry-After: 60`), 503 storage/auth configuration/upstream unavailable. Clients should back off on network/server errors. No private data is reflected in errors.

The production rate-limit binding allows 120 ingestion requests per 60 seconds per ephemeral day-scoped hash of the source IP. No IP or rate key is stored in D1 or application logs. Cloudflare rate limiting is distributed and approximate, not a global billing quota or proof of client identity. Missing limiter configuration fails closed. Ingest is intentionally anonymous and self-reported; an attacker can fabricate device IDs. Apply operator edge abuse controls if needed. Worker request observability is disabled to avoid logging telemetry bodies/headers.

## Metric definitions

| Metric | Definition |
| --- | --- |
| Observed installations | Distinct installation UUIDs in any accepted event; dated by the earliest accepted **event timestamp**, not installer completion or receipt time |
| First opens | Distinct installation UUIDs with a `first_open`; dated by their earliest such event |
| App opens | Number of unique `app_opened` event IDs; repeat deliveries do not increment it |
| DAU / WAU / MAU | Distinct IDs with `active_day` today, today plus preceding 6 days, or today plus preceding 29 days, all UTC |
| Timeline | UTC daily observed installations, first opens, app opens, active devices; 1–90 days, default 30 |
| Version / OS | Each installation counted once using latest event timestamp; UUID lexical order is a deterministic same-second tiebreaker; old offline events cannot downgrade properties |
| D7 / D30 | Exact day-N return based on `active_day`, cohort = earliest observed event UTC day. Denominator includes only installations whose target day ended **before today UTC**; numerator = those active on that day. Empty denominator returns `null`, shown as “—” |

A state reset/reinstall may create another installation ID. These measure devices/state directories, not identified people. Today is partial. Offline uploads can revise prior dates and cohorts. Retention becomes final only after delayed uploads have arrived; the ingestion age window is 30 days, not a database retention policy. Raw records are kept until an operator implements a documented retention/deletion policy.

## Admin dashboard and downloads

Open `/`, type the server's admin bearer token, and select **Load metrics**. **Clear** removes the token and displayed metrics from page memory. Reload also forgets it. Credentials never enter URLs, cookies, browser persistence, HTML, or JS assets. No default admin secret exists. `/api/stats?days=30` and `/api/downloads` require `Authorization: Bearer …`. `ADMIN_TOKEN` must contain 32–256 characters. Same-origin requests only; no CORS authorization is enabled. Responses have `Cache-Control: no-store`, a restrictive CSP, and safe DOM text rendering.

**Load download counts** separately reads public GitHub releases for `craftowen/krill`. Optional server-side `GITHUB_TOKEN` can raise GitHub's read limits; it is never sent to the browser or returned in errors. This request does not block app statistics if GitHub is unavailable. It fetches at most five pages of 20 releases (100 releases maximum), a total 10-second timeout, at most 2 MiB/page, and at most 1,000 assets/release. Pagination URLs are constructed against GitHub's fixed origin, not followed from response links. Truncation is explicitly labeled. Published prereleases are included; drafts are excluded.

Only binary names produced by `packaging/` count: `Krill-<version>.dmg`, architecture `*-adhoc.zip`, Windows x64 ZIP, and Linux architecture `.tar.gz`. Scripts, manifests, checksums, and source archives are excluded. Counts are cumulative binary download **requests across the fetched releases**, including repeats and updates; they are not installs or unique users and have no daily time series. GitHub releases/assets deleted upstream are outside this coverage.

The web dashboard reuses the actual DARK semantic palette, system font, `TEXT_MD`/`TEXT_LG`, `RADIUS_SM`, and spacing from [the style guide](../../docs/STYLEGUIDE.md); it does not embed desktop-only controls.

## Local tests and real SQLite server

Node 22.13+ is required for `node:sqlite`; Node 26 was used during implementation. No npm dependencies are required for the Node tests or local HTTP adapter.

```sh
cd services/telemetry
npm test
```

Create an ignored local token file, then start a persistent real SQLite collector bound to loopback only:

```sh
umask 077
printf 'ADMIN_TOKEN=' > .dev.vars
openssl rand -hex 32 >> .dev.vars
set -a
. ./.dev.vars
set +a
TELEMETRY_DB=/tmp/krill-telemetry-local.sqlite PORT=8787 npm run local
```

The local adapter executes the same Worker handler, SQL, triggers, schema validation, authentication and metrics. It replaces only the D1 binding and distributed limiter with Node SQLite and an in-memory local request counter. It never creates remote resources. Store `TELEMETRY_DB` wherever local test data is appropriate; it is not a Cargo target.

In a second shell, load `.dev.vars` as above and run:

```sh
COLLECTOR_URL=http://127.0.0.1:8787 npm run test:http
```

This writes synthetic events, retries them, verifies rollback on a conflicting duplicate, checks unauthenticated rejection, and reads authenticated metrics. The test refuses non-loopback endpoints. Open `http://127.0.0.1:8787/` and use the local token for dashboard checks. Node tests additionally cover timestamp/UTC boundaries, complete-day retention denominators, out-of-order upgrades, body/field bounds, authentication, failures, downloads, and SQLite durability after reopen.

For Rust → collector E2E, start this server or Wrangler below; point the Rust client's `KRILL_TELEMETRY_ENDPOINT` at `http://127.0.0.1:8787/v1/telemetry`, use isolated `KRILL_STATE_DIR`, and keep `CARGO_TARGET_DIR=/Volumes/krill-build/targets/shared` with Cargo `--locked`. Trigger desktop launch/activity events (or the telemetry core's local integration example), flush, then inspect the authenticated collector stats. A CLI/daemon-only start should not create desktop activity. Opt-out environments can intentionally prevent delivery; inspect `krill telemetry status --json` when testing.

## Wrangler local D1

Use an installed Wrangler 4 CLI. All commands below are local:

```sh
wrangler d1 migrations apply krill-telemetry --local --persist-to /tmp/krill-telemetry-d1
wrangler dev --local --ip 127.0.0.1 --port 8787 --persist-to /tmp/krill-telemetry-d1
```

Wrangler reads ignored `.dev.vars` for `ADMIN_TOKEN`. The descriptive placeholder D1 ID works locally and must be replaced for production. If an older installed workerd rejects `compatibility_date: 2026-10-27`, upgrade Wrangler or use a supported date **only for local execution**, for example `wrangler dev --local --compatibility-date 2026-09-28 …`. The checked-in production date stays current. Run the same `test:http` path against Wrangler to verify real Worker+D1 ingestion, deduplication, transaction rollback and authenticated metrics.

`wrangler types` can generate an ignored local `worker-configuration.d.ts`; no manually maintained binding interface is used. The checked-in config uses the official portable Wrangler schema reference. Local bindings/schema can be validated without deploying.

## Production operator steps (not performed)

These commands create/modify remote resources and are provided for the operator, not executed by this implementation:

1. Select the intended Cloudflare account and ensure it controls `krill.page`. Install an up-to-date Wrangler. Review billing/edge controls in that account.
2. Run `wrangler d1 create krill-telemetry`. Copy its returned database ID into `wrangler.jsonc`, replacing `REPLACE_WITH_D1_DATABASE_ID`. Keep `DB`, database name, and migration directory aligned.
3. Apply `wrangler d1 migrations apply krill-telemetry --remote`.
4. Provision a generated 32–256-character admin secret with `wrangler secret put ADMIN_TOKEN` through its secure prompt. Optionally provision `GITHUB_TOKEN` the same way, limited to public repository read access. Never put either value in config/source/URLs.
5. Review the route `telemetry.krill.page` (`custom_domain: true`), rate-limit namespace `1001` (choose an account-unique namespace if already used), disabled `workers_dev`/preview URLs, and disabled request observability. No production ingest key belongs in a desktop build.
6. Validate with `wrangler deploy --dry-run --outdir /tmp/krill-telemetry-preview`, then explicitly deploy with `wrangler deploy` when authorized. The placeholder must already be replaced; dry-run alone does not prove the remote D1/route/secret exists.
7. Open the HTTPS dashboard and confirm unauthenticated stats return 401, the admin token loads metrics, synthetic current events are acknowledged only after persistence, identical retries are idempotent, and GitHub download failures leave application stats usable. Verify the desktop endpoint only after the collector is live. Publish a desktop release separately when authorized.

The repository does not establish that production collection is working. No deployment, database creation, remote migration, secret installation, GitHub publication, commit, or push is part of this change.

References: [D1 atomic batches](https://developers.cloudflare.com/d1/worker-api/d1-database/#batch), [Workers rate limiting](https://developers.cloudflare.com/workers/runtime-apis/bindings/rate-limit/), [GitHub release API](https://docs.github.com/en/rest/releases/releases#list-releases).
