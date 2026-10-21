# Verification and evidence

Separate implementation success from acceptance. A delivery verifier gets the
original goal, criteria, constraints, changed files, exact code state, and report
paths. Use a fresh worker context for substantive independent verification; do not
ask the implementation worker to certify its own result as independent evidence.
For a trivial change, proportional coordinator review can suffice when consistent
with repository rules. Avoid repeating whole-workspace tests at every small edit.

For code, inspect the diff and exercise the behavior with appropriate focused
checks. For UI behavior, capture the real surface when layout/interaction matters.
For research, verify source claims and provenance; do not build unrelated code or
require screenshots for a cited answer. Evidence must match the criterion.

Use this small report contract (plain Markdown/JSON is sufficient):

- Goal, task/dispatch IDs, write scope, delivered result.
- Criterion: passed/failed/blocked/unverified, plus the concrete observation.
- Code state: HEAD and relevant uncommitted changes, or file/data hashes outside Git.
- Checks: exact command, exit code, scope, and run time; distinguish skipped checks
  and their reasons from passed checks. Do not invent measurements.
- Artifacts: existing paths, what they demonstrate, and hashes when needed for
  future validity. Keep large logs/images out of orchestration message bodies.
- Remaining defects/blockers and the next action. Record who implemented and who
  verified. A failed verifier must not approve integration.

Send a short summary plus `--report-path "<path>" --files-modified "<paths>"`
through `send`. These flags append text to the message; Krill neither uploads nor
validates the report. State storage is bounded. The coordinator must open the
report/artifacts on their execution host and judge the actual evidence.

After code, dependencies, input data, or relevant skill instructions change,
invalidate affected prior passes and rerun those checks against the new state.
An existing report path, old green test, or an unchanged filename alone is not
proof that the current result passes. Keep unaffected evidence when its scope and
inputs still match; do not repeat all work just because one unit changed.

This is an agent workflow contract, not a new mandatory Krill runtime gate.
`worker_done` settles a task and `task-update` can change status directly. Enforce
review ordering through the planned DAG and coordinator decisions; do not claim
the binary cryptographically proves evidence or forces all users through review.
