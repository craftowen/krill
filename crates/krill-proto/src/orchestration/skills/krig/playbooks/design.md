# Design Playbook (Deep Module Design & Parallel Candidate Grafting)

## Trigger

Run in `afk` gear when drawing new module or function boundaries, defining external adapter seams, modeling domain data, or comparing architecture alternatives. Standalone design requests chain into [verify.md](verify.md); implementation pipelines hand the finalized contract to [drive.md](drive.md).

## Steps

1. **Load deep-module criteria.** Read [deepening.md](../references/deepening.md) and weigh the public interface surface (exported functions, argument count, caller ordering constraints) against the complexity hidden inside.
2. **Declare domain data shapes and 4-way dependency taxonomy.** Define core data structures (state machines, registry maps, discriminated unions) before writing code, and classify every external touchpoint into `In-process`, `Local-substitutable`, `Remote-owned`, or `True-external` to fix its boundary-replacement test strategy. For finite-state protocols involving retries, recovery, or async interleaving, also verify invariants and deadlock freedom with [statecheck.mjs](../scripts/statecheck.mjs).
3. **Branch on trivial vs non-trivial boundaries.**
   - **Trivial boundary**: When data shapes and seams are already obvious, finalize a single design without speculative abstraction.
   - **Non-trivial / contested boundary (parallel candidate grafting)**: Launch two independent read-only subagents in parallel under distinct constraints (Candidate A optimizes for minimal public signature surface; Candidate B optimizes for state invariants and branch-free table structure).
4. **Pick a base and graft strengths.** Never blend candidates into a muddled average. Choose the single deeper base by public simplicity, deletion-test survival, and boundary testability, then manually graft at most one proven strength from the rejected candidate.
5. **Hand off the design contract.** Record irreversible architectural boundary decisions as a short `ADR` or module-header contract, and write working design drafts to `.krig/design-spec.md` for downstream `drive` or `verify`.

## Rules

- Do not introduce single-caller pass-through wrappers or speculative interfaces over a single implementation (Two-Adapter Seam Rule).
- On `agy`, run parallel design subagents with `TypeName: "research"` and `Model: "inherit"` in isolated contexts.

## Verification

- Verify the finalized design covers all four elements: (a) core data shapes, (b) public signatures, (c) test strategy per dependency category, and (d) rejected alternatives with rejection reasons.
- Confirm in the final [verify.md](verify.md) cold read that calling contracts and failure boundaries are unambiguous without extra verbal context.
