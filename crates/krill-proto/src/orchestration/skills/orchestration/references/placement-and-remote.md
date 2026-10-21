# Placement and remote execution

An existing folder is a valid workspace; Git is optional. For parallel code edits
use disjoint file ownership or isolated worktrees. `KRILL worktree new <name>
--parent "<base-directory>"` creates an isolated worktree. Pass its exact absolute
path as `worker-start --cwd "<directory>"`; do not assume a newly created worker
inherits another worker's cwd. Preserve the user's dirty worktree and integration owner.

For an already configured remote host, prefix supported commands with
`KRILL --host <user@host> orchestration ...`. A remote start requires an explicit
remote absolute `--cwd`. Resolve executables, reports, Git state, and process
facts on the execution host, not by interpreting remote paths locally.
Do not silently fall back to local execution if the remote command fails.

When a remote run is active, keep that host target on follow-up calls and in the
worker assignment. Network loss is not proof a worker exited. Preserve the
session and reports until the host can confirm its state. Do not copy Orca remote
placement flags into Krill or claim runtime capabilities that this CLI lacks.
