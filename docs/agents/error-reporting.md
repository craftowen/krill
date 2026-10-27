# Error reporting

Error reporting is off by default and has its own consent, separate from usage telemetry. Enable it in **Settings → Resources → Send error reports**, or run:

```sh
krill errors status --json
krill errors enable
krill errors flush --json
krill errors disable
```

Enabling reporting applies to new events. Disabling it clears pending reports. Re-enabling creates a new consent generation; earlier reports are not revived. `KRILL_ERROR_REPORTING_DISABLED`, `DO_NOT_TRACK`, `KRILL_TELEMETRY_DISABLED`, and the existing CI environment vetoes take precedence over consent. A saved consent can remain on while an environment veto stops transmission.

Environment vetoes apply to the process that inherits them. Setting `DO_NOT_TRACK` only in a shell does not change an app already launched from the Dock; use the settings switch or `krill errors disable` for a persistent change. If concurrent consent management keeps cleanup busy, disabling removes the consent immediately but reports a failure so that cleanup can be retried.

## Data sent

Reports contain a random event ID, a closed error code and event kind, Krill component, version/build, operating system, architecture, and an optional relative location in Krill's public source. Responsiveness reports use duration buckets. There is no persistent installation or account identifier and no precise client occurrence timestamp in the network payload.

Reports exclude prompts, terminal contents, command arguments, environment values, credentials, usernames, hostnames, user file paths, raw logs, and diagnostic bundles. A report's kind `panic` means a panic occurred; it does not prove the process terminated. The daemon can recover from a panic.

On macOS, the collector can read bounded local `.ips` files for Krill. It accepts crash reports only when their type, process, and capture time meet the reporting rules. Nonfatal and simulated reports are excluded. It extracts exception and signal enums; frame lists are initially empty. It never sends the original report, memory dumps, symbols, third-party library identifiers, or the operating system's persistent crash reporter identifier. Missing access, unknown formats, and uncertain consent timing cause a report to be skipped. Native crash collection is best effort and happens after the process runs again, rather than at the instant of the crash. Windows and Linux do not have a native OS crash collector in this implementation.

On Unix, long-lived app and daemon instances can leave a locked local execution marker. A later run treats an unlocked, unclean marker from the same consent generation as an uncertain exit, after checking for known panic/native evidence and allowing time for OS reports. This does not identify SIGKILL, memory pressure, shutdown, or a particular cause, and does not mean terminal sessions were lost. Abrupt OS termination can leave this signal even when termination was intentional. The holder and short-lived CLI commands do not create these markers. Windows execution markers are not connected yet.

## Delivery

The app periodically invokes a background CLI uploader while reporting is enabled. The daemon records local events and never performs the network upload in its PTY or session work. For headless use, run `krill errors flush`; opening the app also resumes delivery.

The local queue is limited to 64 events, 4 KiB per event, and seven days of retention. Retry attempts are limited. Network failures, throttling, and server failures retain pending events with backoff. The uploader removes only IDs acknowledged by the server; corrupt local events and individually rejected events can be discarded. Event IDs let the receiver recognize retries. These limits are policies, not measured performance guarantees.

Repeated ordinary errors, panics, and responsiveness reports with the same component, code, public source location, version/build and duration bucket are admitted once per hour. A bounded local cache keeps this suppression after uploads and is deleted when consent is withdrawn. Distinct native crashes and uncertain exits keep their incident IDs. Server counts describe admitted reports rather than every occurrence. Retry delays use the greater of exponential backoff and the server's `Retry-After`, subject to the daily maximum delay.

The default receiver is `https://errors.krill.page/v1/events`. A receiver must be provisioned and deployed before reports can be received; enabling consent alone does not establish a working server. Server deployment is separate from building the desktop app. Queued events remain subject to the local retention and retry limits while the receiver is unavailable.

TLS verification remains enabled. The uploader disables curl configuration-file loading and redirects, and limits request duration, response size, and work per flush. Failed reporting never changes a session command's result. Local diagnostic bundles remain available through `krill doctor --bundle`; creating one does not upload it.

## Receiver contract

`POST /v1/events` accepts a schema-versioned JSON object containing a bounded array of strictly validated events. The response acknowledges committed event IDs through `accepted_ids` and `duplicate_ids`. A partial response acknowledges only the IDs it names; other events remain pending. Unknown response IDs never authorize deleting a local event.

The receiver applies a streaming request-size limit, edge throttling, transactional daily admission and storage caps, and an event-ID uniqueness constraint. Edge throttling is approximate per Cloudflare location and is not a global cost limit. Storage caps use logical rows and bytes; they do not rely on SQLite's physical file size. The receiver hides expired events from queries and deletes them in bounded scheduled batches. Its retention policy is 30 days after receipt.

Event queries require a separate server-only administrator credential. Public desktop builds contain no administrator secret. Application code does not persist request IPs or log report bodies; hosting infrastructure can process connection metadata under its own configuration. Disable request observability when provisioning a receiver. Public admission remains susceptible to quota exhaustion, so deployment must also account for the hosting account's shared request limits.
