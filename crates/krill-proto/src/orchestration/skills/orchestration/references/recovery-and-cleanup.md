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

- `worker-retain <dispatch-id>` keeps the terminal. **For an active dispatch it
  ends ownership and returns its task to ready**; it is not a pause/continue verb.
  Use it on settled work when preserving output or handing ownership back is intended.
- `worker-release <dispatch-id>` archives bounded output and closes its session.
  **Krill also permits this on active work**; the coordinator must not use it as
  routine cleanup before settlement or an explicitly authorized cancellation.
- `worker-stop <dispatch-id>` also closes the session. Closing destroys the running
  worker; preserve needed output and changes first.
- `worker-abandon <dispatch-id>` fences the old attempt, returns its task to ready,
  **and closes its session when present**. This differs from Orca's non-destructive
  abandon. Use only after identifying the exact owned attempt and deciding to end it.
  A late `worker_done` from that attempt is rejected.

For a known failed/stopped attempt, preserve its report, end any still-active
ownership deliberately, verify the task is startable, then `worker-start --run
<run-id> --task <same-task-id> --worker <new-unique-name> ...` with failure context.
Krill has no `--retry-of` linkage; record the prior dispatch in the assignment/report.
Changed code may invalidate a completed dependent verification: make it pending
again with `task-update <task-id> --status pending --summary "<why recheck>"`
and inspect the resulting readiness before dispatching. Do not mark it completed.

Limit retries by the observed failure cause and agreed resource bound. For unknown
state, preserve the session and report uncertainty until ownership and execution
facts are known. Do not use `reset`, kill unrelated sessions, delete worktrees, or
repeat external actions as a shortcut to making the graph look complete.
