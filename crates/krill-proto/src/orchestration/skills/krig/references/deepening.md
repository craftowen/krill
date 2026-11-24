# Deep Module Design Reference (Deepening Reference)

Load this document only when running [design.md](../playbooks/design.md).

## 1. Deep Module Criteria (Ousterhout)

- **Deep module target**: Minimize exported function signatures, configuration parameters, and caller-visible state transitions while maximizing hidden complexity and internal invariant enforcement.
- **Remove shallow wrappers (Deletion Test)**: Merge or delete single-caller pass-through wrappers, forwarding layers that merely relay arguments, and DTOs that mirror internal storage layout verbatim.
- **Two-Adapter Seam Rule**: Introduce an abstract interface only where at least two real adapters exist or boundary replacement is required for testing. Never wrap a single concrete implementation in a speculative interface.

## 2. Four-Way Dependency Taxonomy & Test Strategy

Classify every dependency touched by the design into one of four categories and lock its test strategy:

1. **In-process (pure memory/domain logic)**: Never use mocks or stubs; write direct behavioral tests against real functions and data structures.
2. **Local-substitutable (filesystem, SQLite, local processes, Git)**: Run the real engine against temporary directories or ephemeral/in-memory SQLite instances.
3. **Remote-owned (first-party APIs, controlled workers/services)**: Wrap behind a thin port interface paired with contract tests (request payload and response schema) and an in-memory fake adapter.
4. **True-external (payment rails, third-party SaaS, OS GUI automation)**: Isolate into a single outermost boundary adapter file and pin timeouts, idempotency keys, and explicit failure types as the contract.

## 3. Boundary-Replacement Test Standard

- Never assert call counts or call order of internal private helpers.
- Feed concrete inputs to public module entrypoints and assert only return values, externally persisted state, and payloads observed at boundary adapters. Refactoring internals end to end must keep tests green without touching a single test line.
