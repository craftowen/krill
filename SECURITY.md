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

Known limitation: the documented Gatekeeper bypass used to install a locally/ad-hoc signed, unnotarized macOS build is not itself treated as a privilege-escalation vulnerability. Use the [source-build path](docs/GETTING_STARTED.md#build-from-source-on-macos) if you prefer to build locally. This does not exclude unrelated installer or updater vulnerabilities.

## Secrets and diagnostics

Krill applies redaction rules when creating diagnostic bundles (`[REDACTED]`; see [the diagnostic implementation](crates/krill-proto/src/diag.rs)). Original local panic logs can contain unredacted messages and paths. Review logs and bundles before sharing them; redaction is not a guarantee that every secret is removed. The separate, default-off [error reporting feature](docs/agents/error-reporting.md) sends only a closed metadata schema and does not upload those original files. Treat a missed sensitive pattern as a security report. Never commit the updater's distribution-signing private key.
