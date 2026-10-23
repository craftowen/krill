# Contributing to Krill

[한국어](CONTRIBUTING.ko.md)

Start with the [README](README.md) and [Getting started](docs/GETTING_STARTED.md), then read [AGENTS.md](AGENTS.md), [Architecture](docs/ARCHITECTURE.md), [Roadmap](docs/ROADMAP.md), [Code conventions](docs/CONVENTIONS.md), and [UI style guide](docs/STYLEGUIDE.md). These design and repository rules currently remain in Korean. Architecture, Roadmap, Conventions, and Style Guide define the public repository rules. Internal investigation records stay local.

## Workflow

1. Check existing issues, or open one with a clear completion condition. Keep unrelated changes in separate issues.
2. Base development on `dev`; `main` records validated releases. External contributors submit a pull request from a fork to `dev`. Use one logical change per commit. Start the subject with an issue ID when available (`OWE-873: …`), otherwise an area (`krilld: …`).
3. Validate the affected scope as described below. Fill in the PR template with the tested commit, commands, results, and any limitations.
4. Maintainers integrate completed work into `dev`. Only changes pushed to remote `dev` participate in the nightly release process; uncommitted work, local-only commits, and unmerged feature branches do not. Publishing and protected-ref pushes require explicit authorization; opening a PR is not authorization for an agent to release.

Install the pre-push hooks once per clone with `scripts/install-hooks.sh`. An explicitly authorized protected-ref push records the actual instruction and reason in `KRILL_ALLOW_PROTECTED_PUSH='<user instruction and reason>'`; do not set this as a routine bypass. The [nightly release process](packaging/README.md#야간-자동-배포) pins remote revisions in a separate clone and retains the previous release when its gates fail.

## Checks by change scope

During iteration, run `cargo check -p <crate> --locked` and tests relevant to the changed behavior. Before finishing a logical code change or committing it, check formatting, clippy for affected crates, and relevant tests. Replace `<crate>` with each affected package:

```sh
cargo fmt --all -- --check
cargo clippy -p <crate> --all-targets --locked -- -D warnings
cargo nextest run -p <crate> --locked
git diff --check
```

IPC, shared-protocol, PTY/FFI, or session-survival changes also require integration and survival tests for that path. For Windows code changes, run `scripts/check-windows.sh -p <crate>` (add `--clippy` as appropriate). Record the target HEAD and the exact scope; a partial check is not a full-workspace result.

If **all** changed files are Markdown (`*.md`), or the only other changes are `debug`/`split-debuginfo` settings under Cargo `[profile.*]`, skip Cargo checks and run `git diff --check`. When committing such a change, include `check.sh 생략(<reason>)` in the commit message as required by AGENTS.md. Script, YAML, lint, or other code changes do not qualify for that exception; validate their affected behavior.

The full `scripts/check.sh` is required for a **final release candidate**, rather than every small development change or documentation PR. It covers formatting, clippy, nextest, doctests, dependency policy, third-party notices, and the public surface. Release only the HEAD that passed; a candidate containing Windows changes must also pass `scripts/check-windows.sh --clippy`. CI and other release-specific gates still apply.

## Source-build environment

Use the [source walkthrough](docs/GETTING_STARTED.md#build-from-source-on-macos). Rust's MSRV is 1.98; do not rely on features first introduced in 1.99. Homebrew Rust is the repository's local reference toolchain; rustup is not required. Keep Cargo build output under `/Volumes/krill-build/targets/<name>` in the shared maintainer workspace, including when overriding `CARGO_TARGET_DIR`. Do not create a target directory on the internal SSD.

## Code and review rules

- Declare dependency versions only in root `[workspace.dependencies]`; commit `Cargo.lock` and use `--locked`.
- Keep PTY ownership in `krill-holder`. App or daemon failure must not close live sessions; check the documented [failure boundaries](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범).
- Do not use `unwrap`, `expect`, `panic!`, `todo!`, or `dbg!` in non-test code; narrowly justified invariants use `#[expect(..., reason = "...")]`.
- Limit `unsafe` to holder/daemon FFI modules and the app's platform modules. Every unsafe block needs a `// SAFETY:` explanation.
- UI work follows `docs/STYLEGUIDE.md` and `krill_app::ui` tokens/primitives. Display only implemented functionality.
- Do not change harness files (including AGENTS.md, Conventions, scripts, lint/deny/fmt configuration) without maintainer approval.
- Do not read or copy Zed's GPL-3.0 `terminal`/`terminal_view` code. Permitted Orca source reuse needs source and copyright attribution at the top of the file. Allowed dependency licenses include Apache-2.0, MIT, BSD-2/3, ISC, Unicode-3.0, and Zlib; GPL-family dependencies are prohibited.
- Report measured values with reproduction evidence. Label estimates as estimates; do not turn old benchmark results into new performance claims.

Do not post tokens, keys, private paths, or personal information in changes, logs, or screenshots. If a secret was committed, tell the maintainers promptly; they decide how to repair history. Report vulnerabilities through [Security](SECURITY.md), and follow the [Code of conduct](CODE_OF_CONDUCT.md).
