# Placement and remote execution

An existing folder is a valid workspace; Git is optional. A new worker starts a
new session and uses the current directory by default. Create a child worktree
only when the user asks for it or parallel code edits would cause checkout/file
conflicts. Otherwise use disjoint file ownership in the existing workspace.

Create the child and start its worker in one command:

```text
KRILL orchestration worker-start --run <run-id> --task <task-id> --worktree new-child --name <name> --cwd "<parent-directory>" --agent <configured-profile> --prompt "<assignment>" --json
```

`--name` is required and also names the worker. The worktree containing `--cwd`
is the parent; omit `--cwd` to use the current directory. The child's base defaults
to the parent's current branch, unless `--base <ref>` is provided. If the parent
is detached or its branch cannot be read, the existing repository/daemon default
applies. `KRILL worktree new <name> --parent "<parent-directory>"` uses the same rule.
Pass `--setup run|skip` and `--trust-setup <hash>` just as with `worktree new` when
setup approval is needed. If worker startup fails after creation, the child stays
on disk and the error includes its path; inspect it before retrying.

`--worktree current` and `--worktree "<existing-path>"` reuse an existing workspace.
Local cwd paths are resolved to absolute directories before session creation.
Do not assume a newly created worker inherits another worker's cwd. Preserve the
user's dirty worktree and integration owner.

For an already configured remote host, prefix supported commands with
`KRILL --host <user@host> orchestration ...`. A remote start requires an explicit
remote absolute `--cwd`. `--worktree new-child` cannot be combined with `--host`;
create the remote worktree separately and pass its exact remote path. Resolve executables, reports, Git state, and process
facts on the execution host, not by interpreting remote paths locally.
Do not silently fall back to local execution if the remote command fails.

When a remote run is active, keep that host target on follow-up calls and in the
worker assignment. Network loss is not proof a worker exited. Preserve the
session and reports until the host can confirm its state. Do not copy Orca remote
placement flags into Krill or claim runtime capabilities that this CLI lacks.
