# Slim Playbook (Repository-Wide Bloat Audit & Technical Debt Ledger)

## Trigger

Run in `afk` gear to audit the repository for over-abstraction, unused dependencies, single-re-export barrels, and dead internal modules (`audit` submode), or to collect scattered tech-debt comments and temporary workarounds with rot-risk scoring (`debt` submode). Audit-only requests end without editing files; when actual reduction is also requested, pass the confirmed deletion list to [drive.md](drive.md).

## Steps

1. **Run deterministic slim and debt scanner.** Execute [slim.mjs](../scripts/slim.mjs) to extract the comment-token debt ledger (`debtLedger`), unused `package.json` dependencies (`unusedDependencies`), pure re-export barrels (`passThroughModules`), unreferenced internal modules (`unreferencedModules`), and high-complexity files (`hotspots`):

```bash
node "<SKILL_DIR>/scripts/slim.mjs" --json --repo "$PWD"
```

2. **Rank candidates by the 5-tier slimming priority (`audit` submode or full reduction).** Cross-check scanner hits and hotspots against real code context to filter false positives (such as dynamic plugin loaders or external reflection entrypoints), then order remaining items:
   - **Priority 1 (`delete`)**: Orphan modules with zero production callers, dead feature branches, and unused package dependencies.
   - **Priority 2 (`stdlib`)**: Custom utilities or third-party dependencies replaceable by a few lines of standard library (`node:crypto`, `structuredClone`, `node:test`, `URL`, `Intl`, etc.).
   - **Priority 3 (`native`)**: Application layers reinventing platform or runtime primitives (SQL constraints, native CSS, atomic OS file renames).
   - **Priority 4 (`yagni`)**: Speculative interfaces with one implementation, never-changed config toggles, and internal pass-through barrel files.
   - **Priority 5 (`shrink`)**: Bloated hotspot modules whose repetitive branches collapse into data tables.
3. **Evaluate debt-ledger rot risk (`debt` submode or comment cleanup).**
   - Intentional simplifications or temporary workarounds must specify both a current limit and a trigger condition (`// krig-debt: <current limit> -> <trigger condition and replacement>`).
   - Flag vague deferrals lacking a trigger condition (`rotRisk: true`) so they are either fixed immediately or annotated with a concrete threshold.

## Rules

- For audit, scan, or debt-ledger requests, do not modify source files; report `file:line`, removal target, and replacement concisely in one line per item.
- Never delete an item absent from the static import graph before confirming it is not exposed via package `exports` or invoked as a CLI script from `package.json`.

## Verification

- Verify `slim.mjs --repo "$PWD"` exits `0` and cross-check each reported candidate's call sites via [recon.mjs](../scripts/recon.mjs) or repository search.
- When applying code reductions, verify `slim.mjs --strict --repo "$PWD"` and the downstream `drive`, `demock`, and `verify` test passes.
