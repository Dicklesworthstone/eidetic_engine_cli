# ADR 0015: Forbidden Dependencies Audit as Test

Status: accepted
Date: 2026-05-05

## Context

AGENTS.md specifies forbidden dependencies that must never appear in the
dependency tree:

- Runtime crates: `tokio`, `tokio-util`, `async-std`, `smol` (Asupersync is the runtime)
- Database crates: `rusqlite`, `sqlx`, `diesel`, `sea-orm` (SQLModel/FrankenSQLite is the ORM)
- Graph crates: `petgraph` (FrankenNetworkX is the graph layer)
- HTTP crates: `hyper`, `axum`, `tower`, `reqwest` (no HTTP in core)

Additionally, ADR 0011 (Mechanical Boundary) forbids LLM client crates in the
core binary.

The risk is that a transitive dependency or feature flag silently pulls in a
forbidden crate. Without automated enforcement, the rule is advisory.

## Decision

**Forbidden dependency checks are integration tests that fail CI.**

1. `tests/forbidden_deps.rs` shells out to `cargo tree --edges normal,build,dev`
   and parses the output.
2. The test maintains two lists:
   - `FORBIDDEN_CRATES`: runtime, database, graph, HTTP crates
   - `FORBIDDEN_AI_CRATES`: LLM client libraries (openai, anthropic, etc.)
3. The test runs twice: once with default features, once with `--all-features`.
4. Any match fails the test with the crate name and the path that introduced it.
5. CI runs this test on every PR; merge is blocked on failure.

## Consequences

- Forbidden deps are enforced, not just documented.
- The test is deterministic and offline (uses cached cargo metadata).
- Adding a new forbidden crate requires updating the test constant.
- Transitive deps are caught: if `foo` depends on `tokio`, we see it.
- Feature flags cannot sneak in forbidden deps without failing the test.

## Rejected Alternatives

- **Manual review of Cargo.lock**: Error-prone; reviewers miss transitive deps.
- **CI script outside test harness**: Harder to run locally; not part of `cargo test`.
- **Workspace-level deny.toml**: Requires additional tooling; the test is self-contained.

## Verification

- `tests/forbidden_deps.rs` is the verification itself.
- `cargo test forbidden_deps` passes on clean tree, fails if a forbidden dep is added.
- CI workflow includes this test in the gate.
