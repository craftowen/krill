# Resume Playbook (Session Recovery, Drift Diagnosis, Safe Pause Checkpoint)

## Trigger

Run in `afk` gear when picking up work from a previous session or another contributor (`pickup`), reconstructing current status, or pausing cleanly with a lossless handoff record (`pause`). When finishing implementation is also requested, chain directly into [drive.md](drive.md) after diagnosis.

## Steps

1. **Diagnose workspace state and drift.** Before changing any line of code, cross-check `.krig/handoff.json`, `.krig/base-sha`, `.krig/decisions.tsv`, `.krig/verified.json`, `git status`, and commit history via [checkpoint.mjs](../scripts/checkpoint.mjs):

```bash
node "<SKILL_DIR>/scripts/checkpoint.mjs" status --json --repo "$PWD"
```

2. **Separate completed vs remaining work.**
   - Cross-check `commitsSinceBase` against `KEEP`/`VERIFIED` rows in `decisions.tsv` so already-verified slices are not repeated.
   - If `driftFlags` contains `PATCH_DRIFT_FROM_HANDOFF`, `HEAD_ADVANCED`, `HEAD_REWRITTEN`, `STALE_VERIFICATION`, `ORPHAN_DECISION_COMMITS`, or `STAGED_KRIG_ARTIFACT`, inspect `git diff` and staging state first and re-verify only invalidated ranges.
   - Never blindly discard uncommitted edits (`gitState.worktree.dirtyFiles`) with `git checkout` or `git reset`; confirm whether they are active in-progress work.
3. **Branch to resume vs safe pause (`save`).**
   - **Resuming work**: follow `recommendedAction`:
     - `RESOLVE_BLOCKER`: check whether `.krig/handoff.json` `blockers` are resolved; if not, report the blocker.
     - `READY_TO_SHIP`: verification stamp is fresh; report ready-to-ship status or proceed to [ship.md](ship.md).
     - `NEEDS_VERIFY`: code changed without a fresh stamp; run [verify.md](verify.md).
     - `CONTINUE_DRIVE`: resume [drive.md](drive.md) from uncommitted work or the next step in `handoff.next`.
     - `CLEAN_START`: no pending work or checkpoint exists; start from the requested first stage.
   - **Pausing / handing off safely**: save verified progress (`--done`), the immediate next step (`--next`), and any external approval or credential blockers (`--blockers`) to `.krig/handoff.json`:

```bash
node "<SKILL_DIR>/scripts/checkpoint.mjs" save --repo "$PWD" \
  --route resume --summary "<current status summary>" \
  --done "<verified item 1|item 2>" --next "<immediate next task>"
```

## Rules

- Never re-run irreversible commands (deletions, billing, external messages, production deploys) during recovery without confirming whether they already took effect.
- Never discard pre-existing uncommitted changes you did not author without explicit user approval.

## Verification

- Verify `checkpoint.mjs status --repo "$PWD"` exits `0` and review `recommendedAction` and `driftFlags`.
- For pause/handoff requests, confirm `.krig/handoff.json` was written and matches the current `patchId`.
