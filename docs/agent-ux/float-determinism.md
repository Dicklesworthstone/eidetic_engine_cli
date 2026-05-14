# Floating-Point Determinism Contract

This document defines the determinism boundary for graph and scoring surfaces
that emit floating-point values. It exists for agents consuming `ee` JSON:
within one run environment, graph output must be stable enough to hash; across
different CPU architectures, exact float bytes are not a portability promise.

## Contract

Same-machine determinism is required. Given the same database, indexes, config,
binary, CPU architecture, and query, machine-facing graph and pack outputs must
be byte-identical across the three-run determinism harness after registered
volatile fields are stripped.

Cross-machine byte identity is not required for float-bearing fields. Different
CPU architectures, compiler codegen, or math-library implementations may
produce tiny float differences even when the graph topology and algorithm path
are identical.

Cross-machine rank identity is required where scores are part of ordered
machine output. If float scores are quantized to six decimal places, the ordered
memory IDs, node IDs, edge IDs, and section IDs must remain stable across
architectures. If two quantized scores tie, the tie-breaker must use a stable
identifier, not collection iteration order.

## Harness Rules

`scripts/e2e_overhaul/graph_determinism.sh` is the same-machine gate. It runs
each graph-facing JSON surface three times, strips registered volatile fields,
canonicalizes JSON with `jq -S`, and compares hashes within that single
environment.

Cross-architecture comparison is differential testing. It is useful evidence
for finding unstable algorithms, but it is informational by default and must
not fail release gates solely because raw float fields differ. A failing
cross-architecture comparison becomes blocking only when stable rank IDs differ
after score quantization.

The determinism harness must log enough environment context to explain a
comparison: CPU architecture, Rust toolchain version, `ee` binary path or hash,
and the graph surface under test. Timing fields are volatility, not correctness
evidence.

## Implementation Rules

Graph algorithms must sort all externally visible collections by stable keys
before serialization. Acceptable keys include memory IDs, graph snapshot IDs,
relation strings, section names, and deterministic rank numbers. Hash maps,
parallel iterator completion order, filesystem order, and database rows without
an explicit `ORDER BY` are not acceptable output ordering sources.

Float-bearing ranking should emit both the raw score needed for diagnostics and
a deterministic rank/order selected from quantized scores. Quantization must be
documented at the surface boundary; current graph JSON code uses rounded score
helpers for exported scores and deterministic ID tie-breaks for sorted lists.

Approximate graph algorithms must record the sampling decision in a witness:
algorithm name, graph size, threshold, requested and effective sample sizes,
deterministic seed or pivots, and decision-path hash. The witness explains why
the approximate path was selected; it is not a cross-architecture proof of raw
float equality.

## Volatile Fields

The fields below may be stripped before hash comparison because they describe
measurement or runtime environment, not semantic graph content:

- `snapshotRefreshedAt`
- `runDurationMs`
- `witnessElapsedMs`
- `witnessRecordedAt`
- `algorithmStartedAt`

Additions to this set must update `docs/volatile_field_registry.md`,
`src/obs/volatile_fields.rs`, and the shell lists in
`scripts/e2e_overhaul/determinism.sh` and
`scripts/e2e_overhaul/graph_determinism.sh`.

## Agent Guidance

When deciding whether graph output drift is a bug, first compare stable IDs and
rank order. Treat small raw-score differences as an architecture or build
artifact unless they change the quantized order, selected pack items, omitted
frontier, or explanation text. Report both the environment metadata and the
affected stable IDs when filing a determinism issue.
