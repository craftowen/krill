# Demock Playbook (Removing Production Fake Success & Swallowed Exceptions)

## Trigger

Run automatically in `afk` gear immediately after `drive`, or on request to audit production code for fake successes, swallowed exceptions, test doubles, and unimplemented placeholders. Chains into [verify.md](verify.md) on completion.

## Steps

1. **Resolve scan targets and run scanner.**
   - When chained after `drive`, pass the list of modified and newly created files relative to `.krig/base-sha` (excluding deleted files).
   - For a standalone `demock` run, pass the user-specified paths (or `$PWD` when omitted).

```bash
node "<SKILL_DIR>/scripts/scan.mjs" --json <target_paths...>
```

2. **Four-way triage.** If exit code is `3` (candidates found), inspect each hit in context and classify it into one of four buckets:
   - **(a) Test double in production (`test-double-in-prod`)**: `jest.fn`, `sinon`, `unittest.mock`, or hardcoded stub responses on a production code path.
   - **(b) Fake success or permissive security guard (`swallowed-error`, `permissive-guard`)**: Catch blocks that return `true` or an empty success value, or auth branches that allow access when a secret/signature is missing.
   - **(c) Unimplemented placeholder (`placeholder`)**: Concrete implementations containing `NotImplementedError`, `todo!()`, or `throw new Error("TODO...")`.
   - **(d) Intentional non-fatal fallback**: Legitimate, observable contract fallbacks such as ignoring a best-effort cache warm failure.
3. **Write failing test first and fix honestly.**
   - For every (a), (b), and (c) finding, first write a failing behavioral test that exposes the fake success or missing implementation and confirm it fails.
   - Fix by wiring the real dependency adapter or propagating/rejecting an explicit error until the test passes.
   - Only for genuine (d) fallbacks, add a specific `// demock: allow <reason>` or `# demock: allow <reason>` comment directly above `try` or inside the `catch`/`except` block.

## Rules

- Never silence exceptions or return synthetic success values just to make tests pass.
- External service failures and missing auth secrets must always surface as explicit errors that callers can branch on.

## Verification

- Re-run `node "<SKILL_DIR>/scripts/scan.mjs" <target_paths...>` after fixes and verify exit code `0`.
- Confirm the test suite, including the newly added failure-reproduction tests, passes with exit code `0`.
