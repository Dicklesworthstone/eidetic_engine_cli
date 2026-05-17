# ADR 0039: Write-Hot-Path V2 Implementation Gate

Status: proposed
Date: 2026-05-17
Bead: bd-2lsxf.2.3
Builds on: ADR 0013 (Single-Write-Owner Actor), ADR 0017 (Swarm-Scale Resource Governance)

## Context

SRR3 is the proposed write-hot-path upgrade for high-concurrency agent swarms:
wait-free producer admission, WAL group commit, and sharded read-copy-update
publication around the existing single-write-owner contract. It is tempting to
treat SRR3 as a throughput project, but the correctness risk is higher than the
performance opportunity. A faster write path is invalid if it can reorder writes,
skip audit-chain links, publish snapshots after failed fsync, or silently drop a
cancelled request.

ADR 0013 already accepts the single-write-owner actor. The SRR3 work does not
repeal that decision. It may optimize the path into and through the owner, but
the durable system still has one observable write order and one audit chain.

The SRR3.spec child beads created the pre-implementation proof substrate:

- `src/core/write_owner.rs` contains the pure `WriteSpool` model and the
  generated schedule/property harness.
- ADR 0013 contains the SRR3 invariant manifest and maps each invariant to
  concrete executable evidence.
- `tests/fixtures/failure_modes/write_hot_path_cancelled_before_commit.json`
  and `tests/fixtures/failure_modes/write_hot_path_fsync_failure.json` pin the
  public degraded-code contract for the modeled failure paths.
- `docs/degraded_code_taxonomy.md` and `tests/fixtures/failure_modes/README.md`
  catalog those codes.

This ADR defines the gate that future production SRR3 patches must pass before
they modify queue admission, group-commit batching, audit persistence, or
snapshot publication.

## Decision

SRR3 production code may proceed only through an invariant-first gate. A patch
that touches the production write-hot path must cite the SRR3 invariant manifest
in ADR 0013 and must prove it preserves the manifest before relying on
performance measurements.

The gate has five hard rules.

### G1: Audit-chain order is a correctness gate

Every accepted durable write belongs to exactly one batch, and every batch
belongs to a contiguous audit chain. Batch IDs, audit row IDs, job row IDs,
request IDs, and audit subjects must be deterministic from the accepted write
schedule.

An SRR3 implementation may batch writes, but it may not let batching change the
observable durable order. Any new batching API must include a property test that
compares its durable rows and audit-chain hashes against the sequential
reference interpreter in `src/core/write_owner.rs`.

### G2: Per-table parallel writers are rejected

Parallelizing by table, memory kind, workspace, or command surface is not an
acceptable substitute for the single write order. It fragments the audit chain
and makes cross-table transactions hard to explain to an agent reviewing
provenance later.

The approved shape is many producers feeding one ordered write path. Internally,
that path may pre-validate, coalesce idempotent requests, build derived payloads,
or prepare batch metadata concurrently, but the commit point remains serialized
and auditable.

### G3: Cancellation before commit is represented, not erased

A write cancelled before durable commit must not appear in committed memory,
index, graph, job, or audit rows. It must remain visible in recovery state as a
cancelled request when it passed admission, so replay can distinguish
"cancelled by caller" from "lost during crash."

Cancellation after commit is not rollback. The committed row remains durable and
any follow-up compensation must be a separate audited operation.

### G4: Fsync failure prevents snapshot publication

If fsync, WAL durability, or the equivalent FrankenSQLite/SQLModel commit
barrier fails for a batch, the batch is failed. It must not advance snapshot
generation, refresh a read-side pointer, notify derived indexes as current, or
publish context-visible state.

Partial publication is forbidden. If the implementation cannot prove that a
published snapshot excludes failed batches, it must return a high-severity
degraded or error response and leave the previous snapshot generation in place.

### G5: Benchmarks are secondary to the model

SRR3 exists to make swarms faster, but benchmark wins do not justify weakening
the model. A production patch that improves p50 or p99 latency but fails the
sequential-reference/property comparison is rejected. Performance evidence is
reviewed only after the correctness gate is green.

## Mapping To Production APIs

Production SRR3 APIs should map to the executable model in a way future agents
can audit mechanically:

| Production concept | Model concept | Required evidence |
| --- | --- | --- |
| Producer admission | `WriteSpool::enqueue` | Property schedule records producer ID, sequence, kind, payload size, and idempotency key. |
| Cancellation before commit | `WriteSpool::cancel_pending` before `mark_batch_committed` | Cancelled request ID does not appear in durable rows and appears as cancelled recovery state. |
| Group-commit batch | `WriteSpool::next_batch` | Batch request IDs, audit row ID, job row ID, kind, and durability match the reference interpreter. |
| Successful durability barrier | `WriteSpool::mark_batch_committed` | Snapshot generation advances exactly once for the committed batch. |
| Fsync or commit failure | `WriteSpool::mark_batch_failed` | Batch rows become failed, failure reason is recorded, and snapshot generation does not advance. |
| Recovery replay | `WriteSpool::recovery_records` | Replay distinguishes pending, committed, cancelled, and failed records without inventing rows. |
| Audit-chain digest | `srr3_audit_chain_hash` in tests | Hash sequence is byte-identical to the sequential reference interpreter. |

The implementation may rename production types, but it must keep this mapping
obvious in code comments, test names, or docs. A reviewer should not have to
infer which production event corresponds to "fsync failure" or "published
snapshot."

## Implementation Checklist

Before a production SRR3 patch is acceptable, every item below must be answered
with a file path, test name, fixture path, or explicit "not applicable" reason.

1. Invariant manifest: The patch cites ADR 0013's SRR3 invariant manifest in
   the Beads closeout comment or PR/commit message.
2. Sequential comparison: Queue or batch behavior is covered by a property test
   comparing production-equivalent output to the sequential reference
   interpreter.
3. Per-producer FIFO: Tests show one producer's accepted writes cannot reorder
   across batch boundaries.
4. Batch determinism: Tests show batch IDs, request IDs, audit row IDs, job row
   IDs, and audit subjects are deterministic for the same schedule.
5. Cancellation before commit: Tests or fixtures show cancelled pre-commit
   writes are excluded from durable rows and retained in recovery state.
6. Cancellation after commit: The patch documents whether post-commit
   cancellation is ignored or represented as a separate compensation event.
7. Fsync failure: Tests or fixtures show failed batches do not advance snapshot
   generation or publish derived assets.
8. Audit continuity: Tests show audit-chain hashes have no holes and no
   duplicate batch IDs.
9. Snapshot publication: Tests show published snapshot generations are
   monotone and advance only after committed batches.
10. Recovery replay: Tests show crash/replay state preserves pending,
    committed, cancelled, and failed records.
11. Degraded codes: Any new write-hot-path degraded code has a fixture under
    `tests/fixtures/failure_modes/`, a taxonomy row, and generated docs.
12. Metrics classification: Correctness metrics are separated from performance
    metrics. Correctness failures fail the gate; latency and throughput are
    optimization evidence only after correctness passes.
13. Forbidden dependencies: The patch does not introduce Tokio, rusqlite,
    petgraph, or another forbidden dependency to solve concurrency.
14. RCH verification: Cargo tests, clippy, benches, and broad verify gates run
    through RCH only. If RCH is unavailable, the bead remains open with exact
    worker/error evidence.

## Correctness Gates Versus Performance Gates

Correctness gates fail the implementation immediately:

- sequential-reference mismatch;
- per-producer FIFO violation;
- committed durable row for a pre-commit cancelled write;
- snapshot publication after fsync failure;
- non-contiguous audit chain;
- recovery replay that invents or drops admitted requests;
- missing fixture for a new degraded code;
- forbidden dependency introduction.

Performance gates are meaningful only after correctness passes:

- producer admission p50/p99;
- group size distribution;
- WAL fsync amortization;
- read snapshot publication latency;
- queue depth and pending-byte pressure;
- CPU/core utilization;
- cache or index notification delay.

Performance regressions may block release, but they do not authorize weakening
the correctness model.

## Rejected Alternatives

- **Per-table writer actors.** This looks scalable but creates several write
  orders and makes cross-table audit reconstruction ambiguous.
- **SQLite WAL plus retry loops.** WAL is necessary but does not define agent-
  visible ordering, cancellation semantics, or audit continuity under failure.
- **Best-effort cancellation.** Dropping cancelled requests from all recovery
  state makes crash replay indistinguishable from data loss.
- **Publish-then-repair snapshots.** Publishing failed or partially durable
  state makes context packs and search results observe writes that the audit
  chain cannot justify.
- **Benchmark-first rollout.** SRR3 is allowed to be fast only after it is
  model-equivalent to the sequential interpreter.

## Verification

This ADR is satisfied for a production patch when the patch attaches evidence
for the implementation checklist above. Current pre-production evidence lives in:

- `docs/adr/0013-single-write-owner-actor.md`, section "SRR3 Write-Hot-Path
  Invariant Manifest";
- `src/core/write_owner.rs`, `srr3_property_generators_match_reference_interpreter`;
- `src/core/write_owner.rs`, `srr3_fake_runner_*` tests and helpers;
- `tests/fixtures/failure_modes/write_hot_path_cancelled_before_commit.json`;
- `tests/fixtures/failure_modes/write_hot_path_fsync_failure.json`;
- `docs/degraded_code_taxonomy.md` rows for `write_hot_path_cancelled_before_commit`
  and `write_hot_path_fsync_failure`;
- `tests/fixtures/failure_modes/README.md` catalog rows for the same codes.

The expected closeout evidence for this docs/gate bead is static:

```text
git diff --check -- docs/adr/0039-write-hot-path-v2-implementation-gate.md docs/adr/README.md
rg "ADR 0039" docs/adr/README.md
rg "srr3_property_generators_match_reference_interpreter|write_hot_path_fsync_failure|write_hot_path_cancelled_before_commit" docs/adr/0039-write-hot-path-v2-implementation-gate.md
```

No Rust behavior changes are required by this ADR.
