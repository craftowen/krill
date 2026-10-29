# Security policy

[한국어](SECURITY.ko.md)

## Supported versions

| Version | Support |
|---|---|
| Latest `main` | Supported |
| Release tags (after the first release) | Latest tag only |

Before the first formal release, only the latest `main` is supported.

## Report a vulnerability privately

Use [GitHub's private vulnerability report form](https://github.com/craftowen/krill/security/advisories/new). Private vulnerability reporting was enabled and verified in this repository's settings on **2026-10-23**.

If the form is disabled or unavailable, [open an issue asking the owner for a private contact method](https://github.com/craftowen/krill/issues/new?template=feature_request.md&title=Private%20security%20contact%20request). Include only that request. Do **not** include the vulnerability, reproduction steps, affected inputs, logs, or exploit details in that public issue. Wait for a private channel before sending the report. Do not disclose vulnerabilities in public issues, pull requests, or discussions.

In the private report, include:

- Affected binary/component and input.
- Reproduction commands and environment: Krill commit/tag, OS version and architecture, and Rust version for source builds.
- Actual impact, such as code execution, session takeover, or exposure of sensitive information.

The response target is **within 7 days**. Please delay disclosure of technical details until a fix has been released.

## Scope and limitations

In scope: Rust code in `crates/`, `packaging/install.sh`, `packaging/release.sh`, and GitHub Actions workflows.

Known limitation: the documented Gatekeeper bypass used to install a locally/ad-hoc signed, unnotarized macOS build is not itself treated as a privilege-escalation vulnerability. Use the [source-build path](GETTING_STARTED.md#build-from-source-on-macos) if you prefer to build locally. This does not exclude unrelated installer or updater vulnerabilities.

## Secrets and diagnostics

[Usage metrics](services/telemetry/CLIENT.md) are enabled by default without a consent dialog. Events contain a random installation ID, event ID/type/time, app version, OS, and architecture. They exclude prompts, terminal output, commands, repository names, paths, account information, and hardware identifiers. The collector does not persist client IP addresses in its analytics database. `krill telemetry disable`, `DO_NOT_TRACK=1`, and `KRILL_TELEMETRY_DISABLED=1` stop collection; disabling locally does not delete previously received server records. Native error reporting remains a separate opt-in feature. Collector administration requires a server-side secret; never embed that secret in a desktop build.

Krill applies redaction rules when creating diagnostic bundles (`[REDACTED]`; see [the diagnostic implementation](crates/krill-proto/src/diag.rs)). Original local panic logs can contain unredacted messages and paths. Review logs and bundles before sharing them; redaction is not a guarantee that every secret is removed. The separate, default-off [native error reporting feature](docs/agents/error-reporting.md) sends only a closed metadata schema and does not upload those original files. Treat a missed sensitive pattern as a security report. Never commit the updater's distribution-signing private key.

Mobile web technical error reporting runs automatically unless the browser has a saved opt-out, Do Not Track or Global Privacy Control is enabled, or storage cannot safely preserve the reporting preference. Settings can turn collection off while disconnected. Reports contain bounded connection-stage metadata, observed status codes, timings, coarse browser/OS categories, and public JavaScript asset locations, sent directly to the [error receiver](services/errors/README.md) even if the desktop connection fails. Reports exclude raw exception messages and stacks, URLs, cookies, account and pairing credentials, device/relay identifiers, and terminal content. No installation or page identifier is sent; a random connection-attempt ID only correlates errors from that attempt. Unsent reports stay only in memory (up to 32 events, 30 minutes and four attempts), disappear on reload/close, and are never restored from disk. Old browser error outboxes are deleted. Only the reporting preference persists. Turning collection off aborts the diagnostic request and clears pending reports; it does not delete reports already received by the server. See the [mobile error collection notice](https://krill.page/privacy#mobile-errors) for processing and infrastructure details.
