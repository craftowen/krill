# Task DAG commands

Use `run-create --title`, `run-use <run-id>`, `run-current --json`, and
`run-show <run-id> --json` to recover existing state before creating duplicates.

Create a task with `task-create --run <run-id> --title "<unit>" --desc "<spec>"
--json`. Use `--deps <comma-separated-task-ids>` for dependent work. Inspect
`task-list --run <run-id> --json`; dispatch only `ready` tasks. Task readiness does
not spawn a worker, and Run completion is not inferred from a shell exiting.

For a delivery requiring review, the verification task depends on implementation;
integration depends on verification. A verifier that finds a failure reports
`failed`, leaving integration blocked. Record fixes as new work, then arrange a new
verification attempt against the updated result. Do not manually mark verification
completed just to release a dependency.

`worker-start --run <run-id> --task <task-id> --worker <unique-name> --agent <profile>
--cwd "<directory>" --prompt "<assignment>" --json` creates a dispatch. Keep its
returned ID and session ID. `--spec "<text>"` can create a task automatically;
reuse `--task` for a known task rather than accidentally creating another one.
`--preamble full|minimal|none` controls injection; disabling it transfers the
obligation to supply identity and reporting instructions to the coordinator.

A non-zero start may still have created a dispatch/session and failed only during
readiness or prompt injection. Read the receipt and inspect `worker-list`,
`worker-show`, and `worker-read` before any replacement. A timeout is not proof
that nothing ran. See [recovery-and-cleanup](recovery-and-cleanup.md).

The current Run is shared per Krill state directory. Explicit `--run` and globally
distinct worker names reduce accidental cross-run confusion. Pass `--run <run-id>`
to `check` as well, and validate every message's run/task/dispatch before acting.
