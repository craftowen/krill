# Usage metrics

Krill usage metrics are enabled by default. Installing and running the app does not open a separate consent dialog. Error reporting is independent and remains off until explicitly enabled; see [error reporting](../../docs/agents/error-reporting.md).

## Data and controls

The closed usage-event schema contains only a random installation UUID, random event UUID, event name, Unix timestamp, app version, OS, and CPU architecture. It contains no prompts, terminal output, commands, repository names, local paths, account details, hardware identifiers, or advertising identifiers. An installation ID persists across launches in the local state directory. A different computer or reset state directory is a different installation; these metrics measure installations, not identified people.

```sh
krill telemetry status --json
krill telemetry disable
krill telemetry enable
krill telemetry flush --json
```

`DO_NOT_TRACK=1`, `KRILL_TELEMETRY_DISABLED=1`, and CI detection override the saved setting. Disabling usage metrics clears pending usage events. It does not delete records already received by the collector and does not enable or disable the separate error-reporting preference.

## Event meanings

| Event | Meaning | Counting rule |
|---|---|---|
| `first_open` | First observed app launch with usage metrics enabled | One per installation; existing users first observed after rollout are included |
| `app_opened` | Desktop app launch | App sessions, not daemon starts or CLI attachments |
| `active_day` | User interaction with the desktop app on a UTC date | At most one installation per date |

The daemon remaining alive, terminal output, and an idle desktop timer are not activity. CLI-only and mobile-only use are outside this desktop metric's coverage. A first launch is evidence of an app having run, not proof of a newly completed installer. The collector distinguishes installations observed in any event from installations with a `first_open` event.

Activity signals are coalesced and processed in the background. Quitting before processing or changing the opt-out setting can discard pending input signals; counts are best-effort observations. A policy change fences pending inputs as well as uploads, so disabling and re-enabling does not revive earlier input.

## Delivery

Usage recording and upload run outside the UI and PTY processing paths. Pending events have a fixed bound and age limit. Retries reuse event IDs, and the collector deduplicates them before acknowledging a batch. Network failure must not interrupt terminal sessions. A local queue or an enabled setting is not evidence that a server received the data.

The default endpoint is `https://telemetry.krill.page/v1/telemetry`. `KRILL_TELEMETRY_ENDPOINT` overrides it; HTTPS is required except for explicit loopback HTTP addresses used in local testing. No collector admin secret is included in a desktop build.

The collector implementation is in [services/telemetry](README.md). The production hostname, database, and admin authentication were provisioned on 2026-10-27. Collection requires a desktop version with usage events enabled; older versions do not acquire that capability from a server deployment. Queued events retry with their original event IDs. Inspect CLI delivery state rather than interpreting missing data or time before collection began as zero usage.

## Operator metrics

The collector provides an authenticated dashboard and statistics API. The administrator supplies the server-side admin token; event ingestion does not grant access to statistics.

- Installation totals and first-open totals are separate.
- DAU, WAU, and MAU count distinct installation IDs with `active_day` in the stated UTC windows.
- Daily timelines use UTC dates, so a user's local midnight can differ.
- Retention uses mature cohorts and displays the eligible denominator. Newly observed installations that have not reached the return window are excluded.
- Version and OS breakdowns describe installations; version changes must not add a new installation.
- GitHub release download counts include repeat downloads, updates, testing, and publication verification. They are displayed separately and never treated as installations or users.

See the [collector operations guide](README.md) for local startup, database migrations, authentication, and deployment configuration.
