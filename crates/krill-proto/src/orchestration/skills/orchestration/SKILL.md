---
name: orchestration
description: Supervise parallel AI sessions through Krill task DAGs, worker messages, questions, independent verification, and recovery. Use for requested Krill coordination or an injected Krill worker assignment; ordinary single-session work needs no orchestration.
---

# Krill orchestration

<!-- Coordination design reference: Orca (MIT, Copyright (c) 2026 Lovecast Inc.).
Krill-specific command contracts and workflow guidance are independently authored. -->

Krill runs external AI CLIs. The coordinator chooses the work strategy; Krill records
Run, Task, Dispatch, messages, and sessions. Use the Krill CLI for this workflow,
including from agy/Antigravity and pi. Native subagents and Orca commands do not register
Krill workers and cannot replace these operations.

## Select the role and executable

- `[MK ORCHESTRATION PREAMBLE]` or `[Krill Worker] run=... task=... dispatch=...`
  means **worker**. Follow that assignment and its exact IDs; do not become a coordinator.
- An explicit request to supervise or divide work means **coordinator**.
- Otherwise continue ordinary work. Merely installing this skill grants no authority
  to spawn agents, publish, spend money, or change unrelated files.

In Krill sessions `KRILL_CLI` is the absolute executable path. Invoke it using the
current shell: POSIX `"$KRILL_CLI"`, PowerShell `& $env:KRILL_CLI`, cmd.exe
`"%KRILL_CLI%"`. If absent, resolve `krill` on PATH. Below, `KRILL` means that
resolved executable, not a literal command or a new environment variable.
Keep using the same executable and execution host. If unavailable, report the exact
failure; do not silently switch to Orca, an unrelated Krill, or native subagents.

Krill synchronizes this package and its references on normal startup and before
agent launch; no installation page is required. Busy/conflicting targets are logged
and retried on a later launch. An explicit `KRILL_STATE_DIR` keeps automatic setup
isolated; `skills install` remains available for deliberate setup there. Updating
files does not hot-reload an already running model's conversation. For the guide
matched to the actual executable, use `KRILL skills get orchestration`; load only
needed references with `--reference references/<file>.md` (`--references` lists them).

## Coordinator loop

1. Define the requested result and observable acceptance. For a small bounded task,
   use one worker or work directly when authorized; parallelism is useful only for
   independent scopes. Read [planning-and-routing](references/planning-and-routing.md)
   for decomposition, role profiles, and workload limits.
2. Create/select a Run: `KRILL orchestration run-create --title "<objective>" --json`,
   `run-use <run-id>`, `run-current --json`. Pass `--run <run-id>` on subsequent commands
   that support it; the current Run is shared state, not private coordinator memory.
3. Define tasks before dispatch with `task-create --run <run-id> --title "<unit>"
   --desc "<self-contained assignment>" [--deps <task-id,...>] --json`.
   For delivery work needing independent verification, create implementation,
   verification, and integration tasks with those dependencies. Worker success alone
   must not authorize integration. Research tasks pass on cited findings instead of
   unrelated code changes or GUI checks.
4. Start the ready independent wave before waiting:
   `KRILL orchestration worker-start --run <run-id> --task <task-id> --worker <name>
   --agent <configured-profile> --cwd "<absolute-directory>" --prompt "<assignment>" --json`.
   Use distinct worker names across active Runs. `--account <id>` is optional;
   `--preamble full|minimal|none` controls instructions. Keep `full` for new workers.
   A new worker is a new session; it does not create a new worktree by default.
   Create a child worktree only when the user requests one or parallel code edits
   would conflict in the same checkout/files. Use `--worktree new-child --name <name>`;
   the enclosing worktree of `--cwd` (default: current directory) becomes its parent.
   Example: `KRILL orchestration worker-start --run <run-id> --task <task-id> --worktree new-child --name <name> --agent <configured-profile> --prompt "<assignment>" --json`.
   The base defaults to the parent branch. See [placement-and-remote](references/placement-and-remote.md)
   for setup approval, explicit base selection, and remote limits.
   Optional `--model <id>` and `--effort <level>` override the profile's launch flags
   when supported by the provider (`claude`, `codex`, `pi` for effort); do not pass
   Orca's `--retry-of`.
5. Inspect `task-list --run <run-id> --json`, `worker-list --run <run-id> --json`
   (which includes live `session_state`, `terminal_state`, and `next_action` hints),
   and bounded `worker-read <dispatch-id> --lines 60 --json`. Wait with
   `check --to coordinator --run <run-id> --unread --wait --timeout-ms 60000 --json`
   (plain `--wait` defaults to 60s, max 300s).
   See [messaging-and-gates](references/messaging-and-gates.md) before acknowledging mail
   with `--ack` or `--ack-ids <id1,id2,...>`.
   Reply to questions using `reply <message-id> --body "<answer>" --json`.
6. Read the returned evidence and run the acceptance checks appropriate to the task.
   Use [verification-and-evidence](references/verification-and-evidence.md) for independent
   verification and invalidating stale results. Correct drift through follow-ups or a
   new scoped assignment; do not silently expand the requested result.
7. Resolve every owned dispatch, recording failed/blocked work honestly. Use
   `worker-retain <dispatch-id>` for a needed settled terminal, otherwise
   `worker-release <dispatch-id>`. `worker-abandon <dispatch-id>` ends an old
   attempt and closes its session; it is not a harmless observation command.
   Read [recovery-and-cleanup](references/recovery-and-cleanup.md) before abandoning,
   retrying, or handling an uncertain start. End with each task's outcome, evidence,
   remaining blockers, and retained sessions. Completion means the user's acceptance
   conditions passed, not that the last worker printed “done”.

Commands above with only a subcommand shown still start with `KRILL orchestration`.
Folder workspaces are valid. Use Git-specific checks only when the workspace uses Git.
The CLI is not an automatic scheduler: `ready` means eligible, not already spawned.

## Worker contract

Read [worker-contract](references/worker-contract.md) once for your assignment.
Stay within the assigned scope, read coordinator follow-ups at natural checkpoints,
and record actual evidence. Do not impersonate another worker or reuse a settled ID.

- Ask a blocking question:
  `KRILL orchestration ask --from <worker> --to coordinator --run <run-id>
  --wait --timeout-ms 10000 --summary "<question>" --json`.
  A timeout leaves the question pending. Re-wait with
  `ask --question-id <message-id> --wait --timeout-ms 10000 --json`.
- Finish exactly once, after recording the outcome:
  `KRILL orchestration send --from <worker> --to coordinator --type worker_done
  --run <run-id> --task <task-id> --dispatch <dispatch-id> --outcome succeeded
  --summary "<result and checks>" --report-path "<existing-report>" --json`.
  Use `--outcome failed` for failure, describing the blocker in the report. Omit
  `--task` only when the dispatch has no task and omit report flags when no report exists.
  `worker_done` settles the dispatch and promotes dependencies; never mark a failed
  assignment successful merely to unblock them. Stop the assigned turn after settlement.

## agy, pi, and asynchronous command tools

On agy, use its terminal/command execution tool to invoke the CLI. On pi, use the `bash` tool (or `codemode` for parallel calls) to invoke the CLI. A native
`task`/subagent tool is a different execution system. If the command tool returns
an in-progress handle, use that tool's status/wait operation to collect the final
exit status and JSON. A tool timeout, queued command, or empty interim output is
not a completed Krill command and does not justify a duplicate launch.
Prefer short `check`/`ask` waits and re-wait using the same question ID. Quote paths
with spaces and use the current shell's quoting, not POSIX syntax in PowerShell.
If command output is clipped, read a narrower reference or bounded worker output;
do not infer lifecycle success from a truncated receipt.

## Conditional references

| Situation | Read |
| --- | --- |
| Decomposition, role profiles, parallel write ownership, budgets | [planning-and-routing](references/planning-and-routing.md) |
| Expanded task commands and dependencies | [coordinator-loop](references/coordinator-loop.md) |
| Assigned worker, evidence report, questions and settlement | [worker-contract](references/worker-contract.md) |
| Implementation review, research proof, changed code or missing artifacts | [verification-and-evidence](references/verification-and-evidence.md) |
| Inbox acknowledgment, follow-ups, approvals | [messaging-and-gates](references/messaging-and-gates.md) |
| Uncertain start, interrupted work, retain/release/abandon | [recovery-and-cleanup](references/recovery-and-cleanup.md) |
| Isolated worktree or remote host | [placement-and-remote](references/placement-and-remote.md) |
