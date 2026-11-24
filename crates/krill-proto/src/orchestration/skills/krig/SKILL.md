---
name: krig
description: "Single-entry auto-routing engineering rig. One /krig invocation automatically chains code/lineage/blast-radius recon, pre-reduction alignment, deep module design, finite state-space model checking, scientific implementation/debugging/perf loops (drive), fake-success removal (demock), two-axis independent review with patch stamps (verify), repository slim/debt audits, session recovery (resume), shipping gates (ship), blind A/B evaluation (eval), and structural backprop."
disable-model-invocation: true
---

# krig

## Goal

From a single `/krig <request>` invocation, determine the stage and uncertainty of the task, lazy-load only the required subset of 12 internal playbooks and deterministic CLI levers, and complete scope alignment (`hitl` gear), autonomous execution (`afk` gear), and independent verification end to end.

## Routing Table

| Route | Gear | Playbook | Auto Follow-up Chain | Entry Criteria |
|---|---|---|---|---|
| `align` | `hitl` | [align.md](playbooks/align.md) | none | Ambiguous new specs, inertia requirement reduction, frontier interviews, epic ticket slicing |
| `recon` | `afk` | [recon.md](playbooks/recon.md) | none | Subsystem mechanics (`how`), commit/design lineage (`why`), 5-level pre-change blast radius proof (`blast`) |
| `design` | `afk` | [design.md](playbooks/design.md) | `verify` | Cross-module/function interface design, adapter seams, parallel candidate design grafting |
| `model` | `afk` | [model.md](playbooks/model.md) | `verify` | Finite state-space BFS exploration, deadlock/invariant/reachability checks, shortest-counterexample behavior tests |
| `drive` | `afk` | [drive.md](playbooks/drive.md) | `demock`, `verify` | Concrete ticket implementation (`feature`), runtime binary-search debugging (`bugfix`), single-variable optimization (`hillclimb`) |
| `demock` | `afk` | [demock.md](playbooks/demock.md) | `verify` | Removing production test doubles, swallowed exceptions, permissive guards, and unimplemented placeholders |
| `verify` | `afk` | [verify.md](playbooks/verify.md) | none | Standards/Spec two-axis parallel review, blank-slate cold read, execution-based adjudication, `patch-id` verification stamp |
| `slim` | `afk` | [slim.md](playbooks/slim.md) | none | Repository-wide over-abstraction, unused dependency, and orphan module audit plus `rotRisk` debt ledger |
| `resume` | `afk` | [resume.md](playbooks/resume.md) | none | Interrupted session recovery, patch/commit drift diagnosis, safe pause checkpoint (`handoff.json`) |
| `ship` | `afk` | [ship.md](playbooks/ship.md) | none | `git patch-id` verification freshness check, worktree/secret/conflict hygiene, 1-minute PR brief validation |
| `eval` | `afk` | [eval.md](playbooks/eval.md) | none | Blind A/B oracle evaluation, quantitative skill lift comparison, benchmark leakage audit |
| `backprop` | `afk` | [backprop.md](playbooks/backprop.md) | none | Backpropagating repeated mistakes or user corrections up the enforcement pyramid and into single-source `v0` docs |

To force an explicit route, use the `route:<route> <request>` or `<route>: <request>` prefix (`align`, `recon`, `design`, `model`, `drive`, `demock`, `verify`, `slim`, `resume`, `ship`, `eval`, `backprop`, `full`). A standalone route word or a lowercase route word preceding a ticket ID or file path takes the same precedence.

## Workflow

1. Let `<SKILL_DIR>` be the directory containing this `SKILL.md`. Run the classifier with a single-quoted heredoc (`--stdin`) to avoid shell expansion accidents (backticks, `$` interpolation):

```bash
node "<SKILL_DIR>/scripts/route.mjs" --json --stdin <<'EOF'
<raw user request>
EOF
```

2. Read and execute only the `playbookPath` files listed in the returned `steps` array, in order. Do not preload unselected playbooks or `references/` documents into the main context.
3. Keep all temporary execution state files (`.krig/base-sha`, `.krig/design-spec.md`, `.krig/deletion-ledger.jsonl`, `.krig/decisions.tsv`, `.krig/verified.json`, `.krig/handoff.json`, `.krig/models/`, `.krig/eval/`) under `.krig/` in the workspace, and never stage or commit them to git (`ship-check.mjs` blocks staged `.krig/` files).
4. On entering `drive`, record the baseline commit only when `.krig/base-sha` is empty (`[ -s .krig/base-sha ] || git rev-parse HEAD > .krig/base-sha`) so subsequent `demock`, `verify`, and `ship` stages check the full change set and patch fingerprint consistently against the original base commit.
5. Playbooks that spawn subagents (`align` fact-finding, `recon` parallel exploration, `design` candidate designs, `verify` two-axis review and cold read) follow the active host rules:
   - **Antigravity (`agy`)**: Isolate read-only research and review with `TypeName: "research"`, and always pass `Model: "inherit"` instead of `pro` or `flash`. If a review subagent must print a model name, pass the parent session's actual model name explicitly in the prompt.
   - **Claude Code**: Launch a fresh `Task` (or `Agent`) subagent context restricted to read-only tools and never reuse a prior subagent.
   - **Codex CLI**: Isolate context via the subagent tool or a read-only `codex exec -C "$PWD" -s read-only` invocation.
6. If you discover and fix a recurring mistake pattern during execution, finish with [backprop.md](playbooks/backprop.md) to leave a structural guardrail.

## Rules

- **Gear separation.** Ask for scope reduction and frontier decisions in a single bundled round only in `align` (`hitl` gear, promoted to `afk` when entered via `full`). In `afk` routes (`recon`, `design`, `model`, `drive`, `demock`, `verify`, `slim`, `resume`, `ship`, `eval`, `backprop`, and `align` under `full`), verify reversible choices and observable code/experiment facts directly without interrupting the user. If blocked by missing credentials/permissions or an irreversible policy gap unobservable from code, do not guess; report the blocker as `UNCONFIRMED`.
- **Stop on destructive actions.** Regardless of gear, require explicit confirmation before force-pushing shared branches, deleting production data, deploying to live servers, sending external customer messages, or committing billable actions.
- **Data shapes first.** Name and define core data structures (state machines, registry tables, discriminated unions, boundary parsers) before writing code. Validate inputs once at system boundaries (CLI, network, config parsers) and keep internal functions as pure transformations.
- **Permanent canon limit.** Only four artifact classes belong permanently in git: domain glossaries (`GLOSSARY.md`), irreversible `ADR`s, re-runnable verification/lever scripts, and failure-test corpora.
- **Response format.** Write the final answer once after all tool calls finish, leading with the conclusion (what was done and the verified result) on the first line. Avoid decorative symbols, bullet emojis, middle dots, and long dashes in prose.

## Verification

- Before reporting completion, run every deterministic script required by the active playbook (`ledger.mjs`, `recon.mjs`, `statecheck.mjs`, `decision-log.mjs`, `scan.mjs`, `slim.mjs`, `checkpoint.mjs`, `ship-check.mjs`, `ab-eval.mjs`, `unwrap.mjs`, plus project test and build commands) and verify both exit codes and actual outputs.
- Never claim unverified items as complete; mark them `UNCONFIRMED` with the specific reason.
