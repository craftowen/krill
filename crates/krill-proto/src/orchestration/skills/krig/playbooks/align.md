# Align Playbook (Pre-Reduction, Frontier Alignment, Vertical Slicing)

## Trigger

Run when given an ambiguous new feature spec, a multi-stage project kickoff, scope reduction, or an epic ticket breakdown request. The default gear is `hitl` (human-in-the-loop), switching to `afk` only when entered through the `full` end-to-end pipeline.

## Steps

1. **Separate fact-finding.** Resolve codebase structure, existing schemas, installed tools, and any other inspectable facts through read-only exploration first instead of asking the user.
2. **Trim inertia requirements and isolate `first_nail`.**
   - For every requirement outside legal obligations, external contracts, safety guards, and public compatibility, check its named origin, concrete utility, and cost to add back later.
   - Identify the single riskiest load-bearing assumption (`first_nail`) that could invalidate the rest of the plan, plus the cheapest falsification experiment (prototype, query, or one real API call).
3. **Single-round frontier consensus (`hitl` vs `afk`).**
   - **`hitl` gear**: Present candidate deletions, the `first_nail` falsification experiment, and all unblocked frontier decisions together with recommended defaults in **one bundled round of questions**.
   - **`afk` gear (`full` pipeline)**: Do not stall on questions; adopt the recommended defaults and run the `first_nail` falsification experiment immediately to confirm the core assumption holds.
4. **Record the deletion ledger and lock domain terms.** Log confirmed `deleted`, `kept`, or subsequently `restored` items into `.krig/deletion-ledger.jsonl` via [ledger.mjs](../scripts/ledger.mjs), and record agreed core domain terms in the repository's `GLOSSARY.md`.

```bash
node "<SKILL_DIR>/scripts/ledger.mjs" record .krig/deletion-ledger.jsonl \
  --kind deleted --id "<REQ_ID>" --title "<item>" --source "<origin>" --reason "<reason>"
node "<SKILL_DIR>/scripts/ledger.mjs" record .krig/deletion-ledger.jsonl \
  --kind kept --id "<REQ_ID>" --title "<item>" --source "<origin>" --reason "<keep justification>"
node "<SKILL_DIR>/scripts/ledger.mjs" summary .krig/deletion-ledger.jsonl --json
```

5. **Issue vertical tracer-bullet tickets.** Slice agreed scope into end-to-end verifiable vertical slices rather than horizontal layers (DB, API, UI separately). If an issue tracker (`gh issue`, Linear, Jira, etc.) is configured, register blocking dependencies as well; in `full` mode, feed the first slice in topological order directly into `design` and `drive`.

## Rules

- Never ask the user a question that can be answered by reading code or running an experiment.
- If zero items were deleted (`keptCount > 0` with `deletedCount == 0`) or `addBackRate < 0.1` after deleting 5+ items, review `underDeletionWarning` and re-check speculative requirements.
- Never log `--kind restored` without a prior `deleted` record for the same ID; only restore with concrete evidence of necessity.

## Verification

- Confirm `ledger.mjs summary .krig/deletion-ledger.jsonl --json` aggregates deleted, kept, and restored counts cleanly.
- If new domain terms were introduced, verify `GLOSSARY.md` was updated; if tickets were issued, verify dependency links from the tracker output.
