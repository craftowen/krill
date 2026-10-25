# Planning and role routing

Make each task independently actionable: target, deliverable, constraints, write
ownership, dependencies, acceptance checks, and report location. Forward relevant
repository rules and findings; a new worker does not inherit your conversation.

Choose a shape proportional to the work:

- Research: independent source collection followed by synthesis/verification.
  Require file:line or URL provenance, distinguish observed facts and hypotheses,
  and do not invent implementation changes as a research deliverable.
- Delivery: implementation units containing their related tests, followed by an
  independent verification task and integration after verification passes.
- Review: read-only findings; dispatch fixes only within the user's authorization.
  Recheck affected acceptance criteria after a fix.

Use real dependency edges, not a long artificial chain. Parallel writes must have
disjoint ownership or separate worktrees. For overlapping edits, order the work or
isolate it and name who integrates. Agree on shared interfaces before dependent work.
Never merge or commit someone else's uncommitted changes as your worker's result.

Choose from `KRILL agent list` and the user's configured profiles. A role
such as researcher, implementer, or verifier maps to a configured profile; it is
not a model ID. Respect user model/account choices. Krill has no provider-ranking,
model-price, or cross-provider fallback oracle. `worker-start --agent <profile>`
uses that profile's argv/env, with optional `--model <id>` and `--effort <level>`
overrides when supported by the provider (`claude`, `codex`, `pi` for effort).
Unsupported `--retry-of` must not be used as if it controlled execution. Propose
profile changes if needed, without altering unrelated credentials or silently
switching paid providers.

Set a practical concurrency/retry bound from the task, available profiles, observed
rate limits, resource pressure, and review capacity. Start small when these are
unknown; increase only for independent useful work. Existing `--depth`/`--max-depth`
are recursion limits, not a concurrency or token budget. Do not bypass them with a
new Run. On repeated same-cause failures, stop retrying and report the cause and
remaining choices. Numeric limits in a plan are proposed limits, not measured savings.

The coordinator owns review and integration decisions. Worker “done”, terminal
idle, elapsed time, and a green unrelated test are not substitutes for acceptance.
