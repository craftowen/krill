# Drive Playbook (Scientific Implementation, Binary-Search Debugging, Hillclimbing)

## Trigger

Run in `afk` gear for concrete feature implementation or refactoring (`feature`), bug reproduction and root-cause fixing (`bugfix`), or iterative quantitative optimization (`hillclimb`). Chains automatically into [demock.md](demock.md) and [verify.md](verify.md) on completion.

## Steps

1. **Initialize baseline commit and decision log.** In a git repository, write current `HEAD` to `.krig/base-sha` only if it does not already exist (preserving any existing `.krig/base-sha` carried over from `resume`) so downstream `demock` and `verify` track the full change scope. Read `.krig/design-spec.md` first if present, and initialize `.krig/decisions.tsv` via [decision-log.mjs](../scripts/decision-log.mjs).

```bash
mkdir -p .krig
[ -s .krig/base-sha ] || git rev-parse HEAD >.krig/base-sha 2>/dev/null || true
node "<SKILL_DIR>/scripts/decision-log.mjs" init .krig/decisions.tsv
```

2. **Execute the mode-specific loop (from `route.mjs`'s `driveMode`).**
   - **`feature` mode (implementation and refactoring)**:
     1. Remove dead code or unnecessary complexity first (subtract before adding).
     2. Define core data structures first and implement in vertical slices where each step ends in independent verification.
     3. Pass behavioral tests asserting user-observable outcomes before starting the next slice.
   - **`bugfix` mode (debugging)**:
     1. Reproduce the bug on its real surface (CLI, API, UI, or test) first.
     2. Do not bolt on speculative guards; bisect intermediate runtime state between input and symptom until you locate the first point where behavior diverges.
     3. If two consecutive fixes sharing the same assumption fail, challenge and test the assumption itself.
     4. Confirm a failing test proves the bug before applying the root-cause fix.
   - **`hillclimb` mode (single-variable optimization)**:
     1. Lock the measurement harness before touching code, and verify baseline numbers, run-to-run noise band, the dominant bottleneck (limiter), and zero error count.
     2. Change only one code variable per hypothesis. Never modify the measurement harness mid-loop.
     3. Keep (`KEEP`) only improvements that beat the noise band with zero errors; immediately revert (`REVERT`) changes that fall short or add unjustified complexity.
3. **Accumulate decision entries.** After each experiment or verified slice, record hypothesis, action, observed evidence, verdict (`KEEP`, `REVERT`, `VERIFIED`, `NOT_VERIFIED`, `INCONCLUSIVE`, `NOTE`), and commit hash (omit before committing).

```bash
node "<SKILL_DIR>/scripts/decision-log.mjs" append .krig/decisions.tsv \
  --route drive --step "<step>" --hypothesis "<hypothesis>" --action "<action>" \
  --evidence "<measurement or test output>" --verdict KEEP --commit "<SHA>"
```

## Rules

- Do not pause to ask the user about reversible implementation choices or behavior that can be verified experimentally.
- During iteration, run only module/package-scoped checks and tests; run the heavy workspace-wide check suite once just before completion.
- Respect project and host instructions (`AGENTS.md`, `CLAUDE.md`) for build target directories (e.g., external `--target-dir`) and test runners first.

## Verification

- Run `node "<SKILL_DIR>/scripts/decision-log.mjs" verify .krig/decisions.tsv --repo "$PWD"` (omit `--repo` outside git) and verify exit code `0`.
- Proceed immediately to [demock.md](demock.md) and [verify.md](verify.md) to verify production honesty and spec/standards conformance.
