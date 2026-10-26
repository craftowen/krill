# Worker obligations

Both `[KRILL ORCHESTRATION PREAMBLE]` and `[Krill Worker]` identify a dispatched
worker. Preserve its Run, Task, Dispatch, worker name, and execution host. Load the
Krill guide with the executable in `KRILL_CLI` when available; never replace this
assignment with Orca or native subagent messages.

Work only in the assigned scope. Read existing rules and relevant history, implement
the requested result, and perform the named checks. Report incompatible constraints
or missing information instead of silently widening scope. At natural checkpoints
and immediately before completion inspect
`KRILL orchestration inbox --to <worker> --run <run-id> --json` for follow-ups.

For a blocker, use `ask --from <worker> --to coordinator --run <run-id> --summary
"<question>" --wait --timeout-ms 10000 --json`. Preserve the returned message ID
and re-wait with `ask --question-id <id> --wait --timeout-ms 10000 --json` after
a timeout. In agy/pi collect the command tool's final output before sending another ask.

On long work send a useful status/heartbeat at meaningful checkpoints with the
same Run/Task/Dispatch IDs. It indicates progress, not successful completion.
Before settlement, write a concise result report as specified by
[verification-and-evidence](verification-and-evidence.md). Existing report and
modified-path flags carry metadata, not file contents or automatic verification.

Send one `worker_done` with explicit `--outcome succeeded` or `--outcome failed`,
all assigned IDs, a concise summary, and real `--report-path`/`--files-modified`
values when applicable. For blocked work, either keep waiting for a pending
question or settle failed with the blocker; never send `blocked` or `cancelled`
as an outcome. Then end the assignment and idle. New user work requires a new
ownership decision; do not send lifecycle events under the settled dispatch.
