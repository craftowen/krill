# Verify Playbook (Two-Axis Parallel Review, Cold Read, Execution Verification)

## Trigger

Run automatically in `afk` gear as the final gate after `design`, `model`, `drive`, or `demock`, or standalone for code reviews, PR audits, design cold reads, and benchmark leakage checks.

## Steps

1. **Scope and size the verification.**
   - When `.krig/base-sha` exists, inspect `git diff "$(cat .krig/base-sha)"` (including newly created untracked files); for standalone invocations, inspect working-tree `git diff`, branch divergence `git diff <base>...HEAD`, or the user-specified files/design docs.
   - Trivial one- or two-line typo/config fixes may rely on direct test and build verification; module boundary changes, multi-file edits, new designs, and performance work must run the two-axis parallel review and execution checks below.
2. **Two-axis independent parallel subagent review.** Launch two read-only subagents with no shared context (`TypeName: "research"` and `Model: "inherit"` required on `agy`):
   - **Standards Reviewer**: Reads only the diff and [standards-baseline.md](../references/standards-baseline.md) (plus repository `AGENTS.md`) and reports boundary-validation violations and newly introduced Fowler 12 code smells with `file:line` citations.
   - **Spec Reviewer**: Reads only the original user request/acceptance criteria (plus `.krig/design-spec.md` if present) and the diff, reporting missing requirements, scope creep, and edge-case defects with `file:line` citations.
3. **Blank-slate cold read (for docs, designs, and complex interfaces).** Give a fresh read-only subagent only the artifact text with no background explanation, ask it to summarize what the module/doc does, and patch every ambiguity it had to guess.
4. **Benchmark and evaluation leakage audit (perf/eval work only).** Check all 8 items in [leakage-8.md](../references/leakage-8.md) to catch dataset contamination, harness edits, and sub-noise wins.
5. **Adjudicate by execution, never majority vote.** Never adopt a finding just because multiple reviewers mentioned it. Fix only findings confirmed `VERIFIED` via actual command execution, reproduction input, or deterministic line-level tracing, and document rejection reasons for false positives.
6. **Issue the patch verification stamp.** Whenever a git repo has changes (`git diff` or new files) and all project tests and static checks pass, always mint `.krig/verified.json` via [ship-check.mjs](../scripts/ship-check.mjs), even if subagent review was skipped for a trivial edit:

```bash
node "<SKILL_DIR>/scripts/ship-check.mjs" stamp --repo "$PWD" --evidence "<summary of passing tests and scans>"
```

## Rules

- Do not blur Standards and Spec results into a vague average score; keep verified defects and rejected findings cleanly separated.
- Compilation or static type-checking alone never proves behavioral completion; observe actual outputs (CLI runs, tests, DB queries, endpoint responses).

## Verification

- After applying fixes, re-run the project's static gates and test suite and verify exit code `0` with actual output.
- Once all checks pass in a git worktree, record the `git patch-id` stamp via [ship-check.mjs](../scripts/ship-check.mjs) (`node "<SKILL_DIR>/scripts/ship-check.mjs" stamp --repo "$PWD" --evidence "<verification summary>"`, adding `--base <commit>` on already-committed branches without `.krig/base-sha`) so [ship.md](ship.md) can validate freshness.
- Label each verification item clearly as `VERIFIED`, `NOT_VERIFIED`, or `INCONCLUSIVE` (with reason).
