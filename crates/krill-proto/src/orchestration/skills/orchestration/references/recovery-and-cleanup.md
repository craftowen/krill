# Recovery, fencing, and cleanup

Recover the existing Run with `run-show`, `task-list --run`, and `worker-list --run`
before starting new work. Read saved reports, unresolved questions, current code
state, and verification validity. Resume from those facts, not a reconstructed
plan that repeats completed side effects.

After an uncertain start or command-tool timeout, inspect `worker-show <dispatch-id>`
and `worker-read <dispatch-id> --lines 60 --json`. Failure may have occurred after
session creation but before prompt delivery. Inspect whether the assignment was
accepted before re-injecting; never blindly create a duplicate worker. Quiet output,
terminal idle, contact loss, and heartbeat absence do not prove process death.

Use these actual Krill lifecycle semantics:

`worker_done` records the result and settles ownership; it does not close the
terminal. The coordinator owns cleanup. Once the report/output has been read and
no follow-up needs the terminal, release the settled worker as part of the same
workflow. No separate cleanup approval is needed for sessions created by that
dispatch. Keep active, borrowed, reassigned, and uncertain sessions intact.

- `worker-retain <dispatch-id>` keeps the terminal. **For an active dispatch it
  ends ownership and returns its task to ready**; it is not a pause/continue verb.
  Use it on settled work when preserving output or handing ownership back is intended.
- `worker-release <dispatch-id>` archives bounded output and closes its owned session.
  Active dispatches must first settle, stop, or be abandoned. Borrowed and reassigned
  sessions are preserved.
- `worker-stop <dispatch-id>` closes an owned session. Closing destroys the running
  worker; preserve needed output and changes first.
- `worker-abandon <dispatch-id>` fences the old attempt, returns its task to ready,
  while preserving its terminal session. Use only after identifying the exact owned
  attempt and deciding to end its ownership.
  A late `worker_done` from that attempt is rejected.

Before reporting completion, run `worker-list --run <run-id> --json`. For each
owned settled worker with `terminal_state: "reclaimable"` and a created session,
run `worker-release <dispatch-id>` after preserving the needed evidence. Re-read
the list to verify release; a command attempt alone is not proof of closure.
Retain a settled terminal only for a specific continuing need, and include the
dispatch ID and reason in the final response. Report any failed cleanup instead
of claiming that all worker sessions were closed.

For a known failed/stopped attempt, preserve its report, end any still-active
ownership deliberately, verify the task is startable, then `worker-start --run
<run-id> --task <same-task-id> --worker <new-unique-name> ...` with failure context.
Use `--retry-of <dispatch-id>` or `--retry-request <request-id>` to retain the prior
attempt's lineage and retry bound. Preserve failure context in the assignment/report.
Changed code may invalidate a completed dependent verification: make it pending
again with `task-update <task-id> --status pending --summary "<why recheck>"`
and inspect the resulting readiness before dispatching. Do not mark it completed.

Limit retries by the observed failure cause and agreed resource bound. For unknown
state, preserve the session and report uncertainty until ownership and execution
facts are known. Do not use `reset`, kill unrelated sessions, delete worktrees, or
repeat external actions as a shortcut to making the graph look complete.
