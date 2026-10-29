# Recovery, fencing, and cleanup

Recover the existing Run with `run-show`, `task-list --run`, and `worker-list --run`
before starting new work. Read saved reports, unresolved questions, current code
and verification validity. Resume from those facts rather than repeating side effects.

## Uncertain start and request lookup

A non-zero start, command-tool timeout or missing JSON is not proof nothing ran.
Keep every returned Dispatch, request and session ID. Inspect on the original host:

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

If no receipt or request ID arrived, start with `worker-list --run <run-id> --json`
and match the intended Task/worker before assuming the start created nothing.

```text
KRILL orchestration request-show <request-id> --json
KRILL orchestration worker-show <dispatch-id> --json
KRILL orchestration worker-read <dispatch-id> --lines 60 --json
KRILL orchestration task-list --run <run-id> --json
```

`request-show` returns the Dispatch recorded for a worker-start request. It is
not a journal for every mutation. `--retry-request <request-id>` looks up that
predecessor and creates a **new attempt with a new request ID**; it does not replay
or join the original operation. `--retry-of <dispatch-id>` also starts a new
attempt. Do not substitute Orca's request replay protocol or use either flag as
a probe. A missing request record does not prove the process never started.

Startup can fail after session creation or prompt acceptance. Even a `failed`
Dispatch and a `ready` Task do not prove the previous process stopped editing.
Inspect its real execution and resolve conflicting writes before replacement.

## Observations and next actions

`worker-list` and `worker-show` distinguish Dispatch outcome, `session_state`,
`agent_state`, `terminal_state`, and `next_action`. These are different facts:

| Observation | Next decision |
| --- | --- |
| Active assignment, live session | Wait, answer questions, or inspect bounded output |
| Quiet output, idle agent, missing heartbeat, command timeout | Inspect; none proves completion or process death |
| Session state unknown or remote contact lost | Preserve uncertainty and the session; do not launch a duplicate editor |
| Confirmed exit, assignment still active | Record the failure or deliberately fence that exact attempt before retrying |
| Accepted success/failure report, owned session open | Reuse for related work, retain, or release after reading evidence |
| Accepted success/failure report, borrowed session open | Retain; preserve both the report outcome and the existing session |
| Session reassigned | Leave its current owner in control |

A `next_action.command` is a hint, not a new authorization or a liveness proof.
Its printed `krill` must be replaced with the executable chosen for this Run,
with the same execution host. Reinspect ownership before a destructive action.
After repeated empty waits, enumerate `worker-list --run <run-id> --json` and
pending questions instead of waiting blindly or creating another worker.

## Lifecycle semantics

- `worker-retain <dispatch-id>` preserves a settled outcome and keeps its session.
  **On an active Dispatch it ends ownership and returns the Task to ready**;
  it is not a pause/continue verb. Do not retain active work merely to inspect it.
- `worker-release <dispatch-id>` archives bounded output and closes its owned
  session. It refuses active and borrowed Dispatches; reassigned sessions are not
  closed. Read needed reports first and do not replace a refused release with `kill`.
- `worker-stop <dispatch-id>` closes an owned session. This destroys the running
  worker; preserve needed output and changes first. It refuses borrowed sessions.
- `worker-abandon <dispatch-id>` fences orchestration ownership without closing
  the process. A late `worker_done` is rejected, but file edits and external actions
  can continue. It changes the Dispatch outcome to abandoned; use retain, rather
  than abandon, to preserve an already accepted success/failure on a borrowed session.

Ending ownership is not stopping execution. Before assigning overlapping writes,
confirm the former editor stopped or arrange an explicit conflict-free handoff.
Logical Task readiness alone is insufficient.

## Bounded retries and re-verification

For a known failed/stopped attempt, preserve its report, resolve residual execution,
and verify the Task is startable. Then start a replacement on that same Task:

```text
KRILL orchestration worker-start --run <run-id> --task <same-task-id> --worker <new-unique-name> --retry-of <dispatch-id> --agent <profile> --cwd "<directory>" --prompt "<assignment with failure context>" --json
```

The current CLI chooses a fresh worker name when a retry omits `--worker`; an
explicit name is preserved. Older binaries can inherit the predecessor's name,
sharing its mailbox with a still-running process. Always use distinct names for
attempts. Missing launch fields can be inherited; provide placement and the
intended profile explicitly when they matter. Three consecutive session
start failures for a Task open its circuit breaker. A worker's explicit failed
report is a different outcome. Retry flags do not reset that bound; do not route
around it with a new Task or Run. Inspect the cause and report the remaining choices.

When changed code invalidates a completed dependent verification, use
`task-update <task-id> --status pending --summary "<why recheck>"` and inspect
readiness before dispatching a fresh verifier. Do not manually mark it completed.

Before ending supervision, inspect `worker-list --run <run-id> --terminal-state
reclaimable --json` and resolve each owned cleanup decision. Report any remaining
active/unknown work and retained sessions honestly. Do not use `reset`, kill
unrelated sessions, delete worktrees or repeat external actions to make the graph
look complete.
