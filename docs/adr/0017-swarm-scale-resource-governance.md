# ADR 0017: Swarm-Scale Resource Governance

Status: proposed
Date: 2026-05-06

## Context

`ee` is meant to serve many coding agents on one local machine. A large swarm
can create three different kinds of pressure:

1. Read pressure from repeated `context`, `search`, `why`, and pack inspection
   commands.
2. Write pressure from `remember`, `outcome`, import, recorder, curation, and
   maintenance jobs.
3. Derived-asset pressure from index rebuilds, graph refreshes, cache warmups,
   support bundles, and benchmark or eval runs.

The project already has accepted decisions that constrain the answer:

- ADR 0001 keeps the product CLI-first and harness-agnostic.
- ADR 0004 makes Frankensearch the retrieval engine.
- ADR 0007 makes context packs the primary UX.
- ADR 0013 requires a single write owner for durable writes.
- ADR 0016 leaves embedding model selection to Frankensearch.

Those decisions rule out turning `ee` into an agent scheduler, a central web
service, a custom vector store, or a multi-writer database layer. The swarm-scale
problem still needs an explicit resource-governance model so performance work
does not drift into those rejected shapes.

## Decision

`ee` will scale swarms through measured local resource governance, not through
new ownership of agent orchestration.

1. Reads remain direct, concurrent, and CLI-first. A daemon may prewarm or serve
   derived assets, but ordinary read commands must still work as one-shot CLI
   calls.
2. Durable writes continue to flow through the single write owner. Heavy-swarm
   write paths may queue, batch, and coalesce only when idempotency keys,
   ordering, cancellation, and audit records remain explicit.
3. Caches are derived assets. They are generation-checked, redaction-safe,
   bounded by configured memory budgets, and always safe to discard and rebuild
   from FrankenSQLite, Frankensearch indexes, graph snapshots, and config.
4. Performance explainability is part of the contract. Expensive `context`,
   `search`, and `pack` paths should be able to report candidate counts,
   cache state, DB/index generations, pack pruning stages, elapsed timings,
   degradation reasons, and resource-pressure fallbacks.
5. Scale validation uses deterministic fixtures and RCH-offloaded benchmark or
   E2E profiles. Small profiles are suitable for normal CI; large profiles are
   documented for nightly or high-RAM machines.
6. Support bundles should include redacted scale-regression artifacts when they
   exist: fixture manifests, benchmark summaries, cache reports, write-queue
   reports, and performance explain-plan samples.

The initial planning track is:

- `eidetic_engine_cli-fcq1.1`: deterministic scale workload fixtures.
- `eidetic_engine_cli-fcq1.2`: RCH-friendly benchmark harness and budgets.
- `eidetic_engine_cli-fcq1.3`: hotset prewarm and cache-governor behavior.
- `eidetic_engine_cli-fcq1.4`: write spool and backpressure contracts.
- `eidetic_engine_cli-fcq1.5`: query and pack performance explain-plan reports.
- `eidetic_engine_cli-fcq1.6`: swarm contention and recovery E2E coverage.
- `eidetic_engine_cli-fcq1.7`: support-bundle scale regression artifacts.

## Consequences

This makes high-concurrency behavior testable before it becomes a collection of
one-off optimizations. Agents can see whether a slowdown came from retrieval,
packing, graph/cache staleness, write backpressure, index rebuilds, or host
resource pressure.

The daemon becomes an optional accelerator and write coordinator, not a product
requirement for ordinary memory reads. Users on smaller machines still get
deterministic one-shot behavior, while users with 256GB+ RAM and many CPU cores
can opt into larger caches, stress profiles, and daemon-owned write batching.

The design also makes some implementation choices intentionally harder:

- Cache entries need generation metadata and redaction discipline.
- Write batching needs explicit audit and recovery semantics.
- Benchmarks need stable fixtures and careful separation between deterministic
  JSON fields and timing/resource measurements.
- Support bundles need typed manifests so large diagnostic artifacts remain
  useful and safe to share.

## Rejected Alternatives

- **Make `ee` a swarm scheduler.** Agent harnesses own planning and tool
  execution. `ee` stores and retrieves memory.
- **Require the daemon for fast reads.** This would violate the CLI-first
  product boundary and make simple shell workflows fragile.
- **Add a custom vector store, BM25, RRF, or embedding registry.** Frankensearch
  owns retrieval and model choice.
- **Let many processes write directly with retry loops.** ADR 0013 already
  rejects non-deterministic multi-writer behavior.
- **Keep unbounded in-memory caches.** Large machines should be used well, but
  cache budgets and pressure fallbacks must be explicit.
- **Emit profiler-only diagnostics.** Flamegraphs are useful, but agents need
  stable JSON reports and support-bundle artifacts before reaching for external
  profilers.
- **Expose secrets through cache or benchmark artifacts.** Redaction policy
  applies before derived artifacts leave the process boundary.

## Verification

The decision remains true when all of the following are covered:

1. `br dep cycles --json` reports no cycles for the `eidetic_engine_cli-fcq1`
   planning track.
2. Scale fixtures are deterministic by seed and record corpus size, row counts,
   index size, and command mix.
3. Benchmark smoke profiles run through `rch exec -- env
   CARGO_TARGET_DIR=${TMPDIR:-/tmp}/...` and emit stable JSON report shapes.
4. Cache tests cover generation invalidation, memory-budget fallback,
   redaction-safe entries, cache-on/cache-off output equivalence, and stale
   derived assets.
5. Write-governance tests cover idempotency, queue backpressure, cancellation,
   crash recovery, and visible audit/job rows.
6. Performance explain-plan tests prove explain mode does not change selection
   results and does not expose secret-bearing memory content.
7. Swarm E2E tests record per-process command, pid, start/end time, exit code,
   stderr, JSON stdout hash, and artifact paths for read bursts, mixed
   read/write bursts, daemon-present mode, daemon-absent degraded mode, index
   rebuild during readers, and recovery after interruption.
8. Support-bundle tests verify redacted scale artifacts, hashes, and tamper
   detection.
9. Forbidden-dependency audits continue to reject Tokio, rusqlite, petgraph, and
   other banned crates.
