# Recon Playbook (Code Mechanics, Decision Lineage, Blast Radius Proof)

## Trigger

Run in `afk` gear without editing code when explaining how a subsystem works (`how`), tracing why code or constants took their current shape (`why`), or proving blast radius before a change (`blast`). When combined with a modification request, lock safety constraints here first and hand off to [drive.md](drive.md).

## Steps

1. **Extract deterministic anchors.** Before skimming files by guess, run [recon.mjs](../scripts/recon.mjs) to collect definitions, production and test call sites, module importers, external boundary surfaces (`env`, `cli`, `subprocess`, `filesystem`, `network`), and git commit/pickaxe (`git log -S`) history:

```bash
node "<SKILL_DIR>/scripts/recon.mjs" --json --repo "$PWD" --symbol "<symbol_name>"
# Add --path only when narrowing analysis to a specific file or subdirectory:
node "<SKILL_DIR>/scripts/recon.mjs" --json --repo "$PWD" --path "<subpath>"
```

2. **Execute the mode-specific investigation (from `route.mjs`'s `reconMode`).**
   - **`how` mode (mechanics and mental model)**:
     1. Trace actual control flow with `file:line` anchors from entry points through core data transformations, state stores, and external output boundaries.
     2. For wide subsystems, fan out read-only subagents (`TypeName: "research"`, `Model: "inherit"` on `agy`) by layer and synthesize a single mental model in the main context.
   - **`why` mode (decision lineage and constraints)**:
     1. Cross-check `recon.mjs` pickaxe commits with `git show <SHA>` and `git log -L <start>,<end>:<file>` to reconstruct the original problem and constraints.
     2. Strictly label every claim as **Verified Fact** (backed by commit or test), **Inference** (derived from surrounding evidence), or **UNCONFIRMED** (context outside the repository).
     3. Summarize takeaways for future editors in four buckets: `Preserve` (invariants), `Safe to Change`, `Avoid` (failed past paths), and `Hidden Risk` (non-obvious couplings).
   - **`blast` mode (5-level blast-radius proof ladder)**:
     1. Inspect `L1 lexical/definition`, `L2 callers and importers`, and `L3 external boundary contracts` (CLI flags, JSON schemas, DB columns, env vars, plugin manifests) exhaustively via `recon.mjs`.
     2. Go beyond listing callers: isolate the **single load-bearing safety fact** that must hold for the planned change to be safe.
     3. Run an `L4 runtime probe` or `L5 execution proof` (a short read-only check or non-destructive input run) to prove that safety fact holds.

## Rules

- Never modify tracked source or config files during a standalone `recon` run.
- Never present undocumented historical intent as certainty; mark it `Inference` or `UNCONFIRMED`.
- Always report test callers and production callers separately rather than inflating counts.

## Verification

- Verify `recon.mjs` exits `0` and that cited `file:line` references match the current worktree.
- In `blast` mode, record the actual probe command and observed output that verified the load-bearing safety fact.
