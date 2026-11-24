# Coding Standards & Code Smell Baseline (Standards Baseline)

To avoid polluting implementation context, do not load this document during implementation; only the **Standards Reviewer subagent** in [verify.md](../playbooks/verify.md) reads it.

## 1. Structural & Boundary Invariants

1. **Concentrated Boundary Validation**: Parse and narrow external inputs (CLI arguments, env vars, file contents, HTTP/webhook payloads) once at system entry boundaries. Do not repeat defensive null/type checks deep inside internal call chains.
2. **Domain Structuring**: Replace repeated `if/else` dispatch chains across files and coupled boolean flag pairs (`isLoading` + `isDone`) with explicit state machines or registry tables.
3. **Idempotency**: Ensure retryable or resumable commands, install scripts, and state writers converge to the same final state when run twice consecutively.

## 2. Fowler 12 Code Smells Checklist

Report only smells newly introduced in the reviewed diff, with exact `file:line` citations:

1. **Duplicated Logic**: Identical domain rules copied in two or more places where one copy can drift.
2. **Long Parameter List**: Signatures accepting 4+ loose positional parameters instead of a named options/data structure.
3. **Divergent Change**: A single module repeatedly edited for unrelated reasons (parsing, domain rules, storage I/O).
4. **Shotgun Surgery**: Scattered coupling where one policy change forces edits across many branch statements in multiple files.
5. **Feature Envy**: Functions repeatedly reaching into another module's internal fields to compute results.
6. **Data Clumps**: Fields that always travel together without being grouped into a named type or object.
7. **Primitive Obsession**: Raw unvalidated strings or numbers standing in for status codes, identifiers, or dimensioned quantities.
8. **Speculative Generality**: Unused option flags, hooks, or abstract base classes with no current caller.
9. **Middle Man**: Shallow wrappers that forward calls verbatim without adding transformation or invariant checks.
10. **Mutable Shared State**: Shared mutable objects or files overwritten concurrently by multiple actors or loop stages without isolation.
11. **Comments Explaining What**: Redundant comments restating what the code already expresses (`// Step 1`); allow only comments explaining non-obvious "why".
12. **Implementation-Pinned Tests**: Tests asserting internal helper calls or duplicating implementation constants instead of checking user-observable behavior.
