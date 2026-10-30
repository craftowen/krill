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
`minimal` still includes the assignment and IDs. `none`/`--no-preamble` sends no
assignment text at all; it does not send the raw prompt without instructions.

A non-zero start may still have created a dispatch/session and failed only during
readiness or prompt injection. Read the receipt and inspect `worker-list`,
`worker-show`, and `worker-read` before any replacement. A timeout is not proof
that nothing ran. See [recovery-and-cleanup](recovery-and-cleanup.md).
After a recorded start failure, manually resending its old preamble cannot restore
that Dispatch's settlement authority. Resolve residual execution and start a new
attempt rather than directing more work under a failed identity.

The current Run is shared per Krill state directory. Explicit `--run` and globally
distinct worker names reduce accidental cross-run confusion. Pass `--run <run-id>`
to `check` as well, and validate every message's run/task/dispatch before acting.

## Reuse a settled session

For immediate related follow-up, inspect `worker-show <old-dispatch-id> --json`
and confirm the original assignment settled and the agent is ready for input.
Then bind a new Task and Dispatch to the existing numeric session:

```text
KRILL orchestration worker-start --run <run-id> --task <next-task-id> --worker <new-unique-name> --session <session-id> --agent <existing-profile> --cwd "<actual-session-directory>" --prompt "<follow-up>" --json
```

This preserves the agent's conversation; it is unsuitable for independent
verification. `--cwd` records placement and does not move an already running
process. Confirm the real host, directory and agent before sending. Do not use
`--model` or `--effort` with `--session`: launch overrides require a new session.

The new Dispatch **borrows** the session. Krill does not transfer permission to
close it: retain the new settled Dispatch, and do not release the old one to
close a session that has been reassigned. Automatic cleanup authority does not
transfer to the borrowed Dispatch; its existing session remains retained for
its owner. A separate user-authorized session close must target that exact
session after checking it has no new owner. A new Task being ready does not
authorize injecting another assignment into an agent still doing prior work.
