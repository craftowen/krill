# Ship Playbook (Patch-Freshness Shipping Gate & 1-Minute PR Brief)

## Trigger

Run in `afk` gear when turning verified changes into clean commits, opening a PR, or checking final ship readiness right before merge or release.

## Steps

1. **Verify patch freshness and worktree hygiene.** Run [ship-check.mjs](../scripts/ship-check.mjs) to check: (a) no `.krig/` temp artifacts staged in the git index, (b) no merge conflict markers or hardcoded secrets, and (c) the current patch matches the `git patch-id --stable` stamp (`.krig/verified.json`) issued by [verify.md](verify.md):

```bash
node "<SKILL_DIR>/scripts/ship-check.mjs" verify --repo "$PWD" --require-verified
```

2. **Resolve missing or stale verification.**
   - If `MISSING_VERIFICATION_STAMP` or `STALE_VERIFICATION` (even a one-line edit after verification) is reported, stop shipping and re-run [demock.md](demock.md) and [verify.md](verify.md) to mint a fresh stamp.
   - When cleaning up local commits, stage only intended files in logical commits, then re-verify with `--require-clean`:

```bash
node "<SKILL_DIR>/scripts/ship-check.mjs" verify --repo "$PWD" --require-clean --require-verified
```

3. **Draft and validate a 1-minute PR brief (<= 40 lines).** When preparing a PR or ship summary, write a Conventional Commit title (`type(scope): subject`, no trailing period, <= 72 chars) and a <= 40-line body containing the five required sections (`## Why`, `## What changed`, `## Scope`, `## Blast Radius`, `## Verification`), then validate it via `ship-check.mjs brief`:

```bash
node "<SKILL_DIR>/scripts/ship-check.mjs" brief --title "<type(scope): subject>" --stdin <<'EOF'
## Why
<1-2 sentences on the problem and approach>

## What changed
- <1-3 core symbols or boundaries changed>

## Scope
- <Included scope and explicitly deferred follow-ups>

## Blast Radius
<1-2 sentences on affected call boundaries and why the change is safe>

## Verification
- <Commands actually executed and observed outcomes>
EOF
```

4. **Respect remote write boundaries.** Only push or open a PR (`gh pr create`, ready-for-review rather than draft) when the user explicitly instructed it; otherwise prepare the verified commit and validated brief locally and report.

## Rules

- Never claim a prior verification still holds after even a "minor comment or formatting tweak" post-`verify`; always refresh the stamp (note that a pure commit or pure rebase with identical patch content preserves `git patch-id --stable` and the binary/mode digest).
- Before starting an unrelated new task in the same checkout after shipping and merging, remove stale `.krig/base-sha`, `.krig/decisions.tsv`, `.krig/verified.json`, and `.krig/handoff.json` so the new task starts from a fresh `HEAD`.
- Keep PR bodies free of raw 40-char commit-hash lists, agent trial-and-error diaries, and file-by-file change dumps.
- Never run `git push --force` on shared branches or deploy to production without explicit user confirmation.

## Verification

- Confirm `ship-check.mjs verify --repo "$PWD" --require-verified` (plus `--require-clean` when committed) exits `0`.
- When preparing a PR brief, confirm `ship-check.mjs brief` exits `0`.
