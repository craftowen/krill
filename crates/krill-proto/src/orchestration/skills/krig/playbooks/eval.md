# Eval Playbook (Blind A/B Quantitative Evaluation & Skill Lift Proof)

## Trigger

Run in `afk` gear to quantitatively compare outputs produced with a skill/playbook/prompt variant (`with-krig`) against a control (`no-skill`), or to benchmark two agent harness configurations objectively.

## Steps

1. **Configure a blind two-arm experiment.**
   - Lock the execution engine, model, reasoning effort, allowed tools, and timeout identically across both arms.
   - Clone from the same initial git commit into neutral workspace paths such as `/tmp/ws-alpha/<task>` and `/tmp/ws-beta/<task>`.
   - Pass the organic user prompt alone to the control arm (`no-skill`) with skills and slash commands disabled, and pass the identical prompt prefixed with `/krig` to the treatment arm (`with-krig`).
   - Run [ab-eval.mjs](../scripts/ab-eval.mjs) before execution to check for blinding leaks:
     ```bash
     node "<SKILL_DIR>/scripts/ab-eval.mjs" check-blinding --prompt "<organic_prompt>" --dir "<workspace_A>" --dir "<workspace_B>"
     ```
2. **Run in isolation and collect execution artifacts.**
   - Execute both arms in isolated processes so neither can read the other's workspace or grading scripts.
   - Verify whether the treatment arm actually ran routing and verification tools using concrete workspace artifacts (`.krig/` state files, commits/diffs, process logs) rather than agent self-reports.
3. **Grade with held-out deterministic oracles.**
   - After all runs finish, run held-out deterministic oracle scripts stored outside the workspaces identically against both outputs.
   - Record objective metrics rather than subjective LLM grading: passed held-out functional/boundary assertions (`passedAssertions` / `totalAssertions`), silent-failure and invariant defect escapes (`defectEscapes`), production LOC (`prodLoc`), and wall time (`durationMs`).
4. **Compute paired comparison and audit leakage.**
   - Feed both arms' task results into [ab-eval.mjs](../scripts/ab-eval.mjs):
     ```bash
     node "<SKILL_DIR>/scripts/ab-eval.mjs" compare --control "<control.json>" --treatment "<treatment.json>" --model "<model_name>"
     ```
   - Audit all 8 items in [leakage-8.md](../references/leakage-8.md) for dataset contamination, cherry-picking, crash-as-zero distortion, and post-hoc grader edits.

## Rules

- Never expose evaluation-hinting words (`eval`, `benchmark`, `rubric`, `judge`, `candidate`, `treatment`, `control`, `grading`) in workspace paths, filenames, or prompts visible to candidate agents.
- Report paired results (wins, ties, losses, median lift) across the full predefined task set without cherry-picking favorable tasks.
- Always state task count, total assertions, and model name alongside metrics, and never extrapolate benchmark lift to unmeasured repositories as proven numbers.

## Verification

- Confirm `ab-eval.mjs check-blinding` returns exit code `0` (`leaks: []`) for every task prompt and both workspace directories.
- Confirm zero infrastructure or timeout errors across both arms, and attach the actual `ab-eval.mjs compare` table and JSON output as evidence.
