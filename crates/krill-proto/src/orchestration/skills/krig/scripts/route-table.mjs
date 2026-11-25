export const ROUTE_TABLE = Object.freeze([
  Object.freeze({
    id: 'align',
    gear: 'hitl',
    playbook: 'playbooks/align.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/ledger.mjs']),
    chain: Object.freeze([]),
    summary: 'Inertia requirement reduction, first-nail falsification, frontier interview, GLOSSARY update, vertical ticket slicing'
  }),
  Object.freeze({
    id: 'recon',
    gear: 'afk',
    playbook: 'playbooks/recon.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/recon.mjs']),
    chain: Object.freeze([]),
    summary: 'Read-only investigation of subsystem mechanics (how), commit/design lineage (why), and 5-level blast radius proof (blast)'
  }),
  Object.freeze({
    id: 'design',
    gear: 'afk',
    playbook: 'playbooks/design.md',
    references: Object.freeze(['references/deepening.md']),
    scripts: Object.freeze([]),
    chain: Object.freeze(['verify']),
    summary: 'Deep module boundary design, two-adapter seams, 4-way dependency taxonomy, and parallel candidate grafting'
  }),
  Object.freeze({
    id: 'model',
    gear: 'afk',
    playbook: 'playbooks/model.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/statecheck.mjs']),
    chain: Object.freeze(['verify']),
    summary: 'Dependency-free finite state-space BFS exploration, invariant/deadlock/reachability checks, and shortest-counterexample behavior tests'
  }),
  Object.freeze({
    id: 'drive',
    gear: 'afk',
    playbook: 'playbooks/drive.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/decision-log.mjs']),
    chain: Object.freeze(['demock', 'verify']),
    summary: 'Vertical-slice feature work, runtime binary-search bugfixing, and locked-harness single-variable optimization loops'
  }),
  Object.freeze({
    id: 'demock',
    gear: 'afk',
    playbook: 'playbooks/demock.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/scan.mjs']),
    chain: Object.freeze(['verify']),
    summary: 'Remove production test doubles, swallowed exceptions, permissive guards, and unimplemented placeholders'
  }),
  Object.freeze({
    id: 'verify',
    gear: 'afk',
    playbook: 'playbooks/verify.md',
    references: Object.freeze(['references/standards-baseline.md', 'references/leakage-8.md']),
    scripts: Object.freeze(['scripts/ship-check.mjs']),
    chain: Object.freeze([]),
    summary: 'Two-axis Standards/Spec parallel review, blank-slate cold read, execution-based adjudication, and patch-id verification stamp'
  }),
  Object.freeze({
    id: 'slim',
    gear: 'afk',
    playbook: 'playbooks/slim.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/slim.mjs']),
    chain: Object.freeze([]),
    summary: 'Repository-wide over-abstraction, unused dependency, and orphan module audit plus rotRisk tech-debt ledger'
  }),
  Object.freeze({
    id: 'resume',
    gear: 'afk',
    playbook: 'playbooks/resume.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/checkpoint.mjs']),
    chain: Object.freeze([]),
    summary: 'Prior session state recovery, patch/commit drift diagnosis, and lossless pause checkpointing (handoff.json)'
  }),
  Object.freeze({
    id: 'ship',
    gear: 'afk',
    playbook: 'playbooks/ship.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/ship-check.mjs']),
    chain: Object.freeze([]),
    summary: 'git patch-id verification freshness check, worktree/secret/conflict hygiene, and <=40-line 1-minute PR brief validation'
  }),
  Object.freeze({
    id: 'eval',
    gear: 'afk',
    playbook: 'playbooks/eval.md',
    references: Object.freeze(['references/leakage-8.md']),
    scripts: Object.freeze(['scripts/ab-eval.mjs']),
    chain: Object.freeze([]),
    summary: 'Blind two-arm (no-skill vs with-krig) isolation, held-out deterministic oracle grading, and paired lift metrics'
  }),
  Object.freeze({
    id: 'backprop',
    gear: 'afk',
    playbook: 'playbooks/backprop.md',
    references: Object.freeze([]),
    scripts: Object.freeze(['scripts/unwrap.mjs']),
    chain: Object.freeze([]),
    summary: 'Enforcement-pyramid structural fixes and de-tooled single-source v0 documentation backprop'
  })
]);

export const ROUTE_BY_ID = new Map(ROUTE_TABLE.map((entry) => [entry.id, entry]));
