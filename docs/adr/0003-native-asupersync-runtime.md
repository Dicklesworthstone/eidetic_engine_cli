# ADR 0003: Native Asupersync Runtime

Status: accepted
Date: 2026-04-29

## Context

Long-running imports, indexing, graph refreshes, daemon jobs, and adapters need
cancellation, budgets, supervision, and deterministic tests. The project also
has a hard no-Tokio requirement.

## Decision

Asupersync is the runtime foundation for async and supervised work. Runtime
boundaries preserve `&Cx`, request budgets, capability narrowing, and
`Outcome`. Tokio, tokio-util, async-std, and smol are forbidden in the default
dependency tree.

## Consequences

Cancellation and budget behavior are explicit architectural contracts. Runtime
tests can use deterministic Asupersync lab support. Adapter and daemon work must
fit this model rather than smuggling in a separate executor.

Synchronous database operations may remain synchronous. Async is reserved for
edges that benefit from cancellation, supervised concurrency, subprocesses, or
future adapter transports.

## Rejected Alternatives

- Tokio as a general executor.
- Async-std or smol as a lighter runtime.
- Ad hoc thread management without `Outcome` preservation.
- Treating cancellation as ordinary error handling.

## Verification

- Dependency audits fail on Tokio, tokio-util, async-std, and smol.
- Runtime contract tests cover budget exhaustion, cancellation propagation,
  quiescence, and no orphan tasks.
- CLI boundary tests prove `Outcome::Cancelled` and `Outcome::Panicked` map to
  documented exits without partial durable writes.

