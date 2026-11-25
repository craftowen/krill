# Backprop Playbook (Enforcement Pyramid & Single-Source v0 Backprop)

## Trigger

Run in `afk` gear whenever an agent mistake, user correction, gate bypass, or recurring friction is discovered during a session, to leave a permanent structural guardrail.

## Steps

1. **Choose the highest layer of the enforcement pyramid.** Before writing a note in docs, determine whether the failure can be eliminated at a stronger layer in this order:
   - **Level 1 (`architecture`)**: Remove or merge module/path structure so the invalid call or shared state cannot exist.
   - **Level 2 (`type`)**: Encode invariants in schemas or types so illegal states are unrepresentable.
   - **Level 3 (`gate`)**: Block regressions with a static gate or lint rule whose error message includes an immediately runnable `fix`.
   - **Level 4 (`test`)**: Pin the real failure input in a fixture corpus with a behavioral regression test.
   - **Level 5 (`doc`)**: Record in a single canonical document only judgment criteria that cannot be mechanically enforced by Levels 1 through 4.
2. **Prove failure reproduction.** For Level 1 through 4 fixes, verify the new gate or test fails (red) against the buggy input/state and passes (green) after the fix.
3. **De-tool and verify single canonical source.** When Level 5 documentation backprop is required, run [unwrap.mjs](../scripts/unwrap.mjs) to ensure no tool names, people, ticket IDs, or patch-date tags leaked in, and that the target is a real single source rather than a symlinked copy (add `--allow-host-specific` only for host-adapter sections).

```bash
node "<SKILL_DIR>/scripts/unwrap.mjs" check-rule \
  --level doc \
  --target "<canonical_file_path>" \
  --text "<generalized_rule_sentence>"
```

4. **Patch-free `v0` rewrite.** Do not append caveats or addenda onto the end of an existing paragraph; rewrite the affected section cohesively as if the rule had been known from version 0. Use `unwrap.mjs reflow <file> --write` if lossless line-wrapping cleanup is needed.

## Rules

- Do not duplicate in documentation any rule already enforced completely by a structural check (Levels 1 through 4).
- Never copy the same rule across multiple files; keep it in the single canonical file read by the actor performing that work and link to it elsewhere.

## Verification

- Verify `unwrap.mjs check-rule` exits `0`, and confirm the target repository's static checks and test suite (or `npm run check` and `npm test` at the harness root when editing canonical harness docs) exit `0`.
