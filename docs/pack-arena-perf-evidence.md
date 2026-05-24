# Arena-Backed Pack Assembly: Perf Evidence (bd-1prrl.7.5)

This document is the perf-evidence deliverable for bd-1prrl.7.5
(swarmx.12.e — arena allocation and tail-latency perf evidence). It
satisfies the bead acceptance clause that perf results may either
**show improvement** *or* **explicitly document why the arena path
should not be promoted**. The conclusion below is the latter: with the
current request-scoped implementation, there is no allocator delta to
promote yet.

## Goal

Decide whether `ArenaMode::RequestScoped` (introduced by bd-1prrl.7.3
and parity-proved by bd-1prrl.7.4) should be promoted from the
`Disabled` default in production context-pack orchestration based on
measurable allocation and tail-latency improvement on graph-heavy and
provenance-heavy packs.

## Fixture identity

The reference fixtures live in `tests/arena_parity_golden.rs` (added
in 4fe0673a):

| Fixture | Candidate count | Token budget | Selection pressure |
| --- | --- | --- | --- |
| `fixture_empty` | 0 | 1_000 | none — empty pack |
| `fixture_below_budget` | 3 | 4_000 | none — all candidates fit |
| `fixture_budget_pressured` | 24 | 500 | heavy — omissions guaranteed (sanity-checked in `arena_parity_budget_pressured_forces_omissions_balanced`) |
| `fixture_coverage_fill` | 20 | 1_400 | medium — MMR backfill engaged after the diversity-greedy phase exits |
| `fixture_tombstoned_mix` | 8 (4 live + 4 tombstoned) | 2_000 | low — filter and selection both exercised |
| `fixture_provenance_heavy` | 9 | 4_000 (Thorough: 6_000) | low — multiple `PackProvenance` URIs per candidate |

These fixtures already feed bd-1prrl.7.4's parity goldens; reusing them
keeps the measurement aligned with the byte-identical contract that
gates promotion.

## Structural analysis

The 7.2 scratch adapter (`PackDraftScratch`, `MmrAssemblyScratch` in
`src/pack/mod.rs`) is created via
`MmrAssemblyScratch::with_candidate_capacity(candidate_count)` at the
top of `assemble_mmr_draft` and via
`PackDraftScratch::with_candidate_capacity(candidate_count)` at the
top of `assemble_facility_location_draft`. Both call paths construct a
fresh scratch, populate it, and drop it before the function returns.

The 7.3 `ArenaMode` plumbing exposes two modes:

- `Disabled`: identical to the pre-7.3 path.
- `RequestScoped`: same per-request allocation, with a named lifetime
  contract and a tracing `ArenaScope` open/close audit.

Because both modes allocate scratch with the same
`with_candidate_capacity(n)` calls inside the same assembly stack
frame, the expected allocation profile of the two modes is **byte-for-
byte identical** for the same input. The 7.4 parity goldens
(`tests/arena_parity_golden.rs`) prove the *output* parity; the
*allocation* parity follows from the identical call shape.

There is therefore no expected allocation delta to measure between
`ArenaMode::Disabled` and `ArenaMode::RequestScoped` in the current
implementation. Any nonzero delta would be measurement noise.

## Why we are not promoting

Promotion criteria (per `docs/pack-arena-assembly.md`):

> Perf: Allocation count and p95/p99 pack assembly latency improve or
> the feature stays disabled.

Given the structural analysis above:

- **Allocation count delta**: structurally zero. No promotion-worthy
  evidence is possible from the request-scoped mode alone.
- **p95 / p99 pack assembly latency delta**: bounded by the runtime
  cost of one `ArenaScope::new()` + `ArenaScope::drop()` call per
  request, which is two `tracing::trace!` emissions plus an enum copy.
  This is well below pack assembly's dominant cost (similarity
  computation, candidate sorting, MMR/facility-location loops) and is
  not a credible win over Disabled.
- **Graph-heavy / provenance-heavy fixtures**: the provenance-heavy
  fixture (9 candidates × 3-6 URIs each) and the coverage-fill fixture
  (20 candidates with overlapping content) both stress the renderer
  and the selector, but neither path crosses the arena-scoped scratch
  in a way that distinguishes the two modes.

The arena scaffolding is therefore correct and useful as the lifetime
+ parity contract that bd-1prrl.7.4 freezes, but it is not yet a
production switch that should default to `RequestScoped`. Context
orchestration (in `src/core/context.rs` at the
`assemble_draft_with_profile_and_options_seeded` call site, line 1786
as of c8c0ed4b) wires `arena_mode: ArenaMode::Disabled` explicitly to
preserve current behavior.

## Path to a promote-able mode

The route to measurable allocation savings is **`workspace_reuse`**,
deliberately deferred by `docs/pack-arena-assembly.md`:

> `workspace_reuse` is not part of the first implementation. A later
> child may reuse an arena across requests only if it adds explicit
> reset auditing, poisoning on panic or failed reset, capacity caps,
> and a generation key that includes the workspace, schema version,
> resource profile, and arena policy version.

A future child bead implementing `workspace_reuse` should:

1. Reuse `PackDraftScratch` / `MmrAssemblyScratch` instances across
   multiple `assemble_*` calls within one workspace, gated by
   `arena_reuse_generation`.
2. Call `reset_for_candidate_capacity(n)` (already implemented for
   tests by bd-1prrl.7.2) at the start of each reused request.
3. Poison the arena on panic or failed reset and degrade to
   `Disabled` allocation for the rest of the workspace lifetime.
4. Capture allocation deltas via a `GlobalAlloc` wrapper measured
   only in the bench fixture and not in production code.
5. Run the parity goldens from `tests/arena_parity_golden.rs` with
   the new mode to keep the byte-identical contract; any regression
   trips a focused test before broad CI.

At that point, the perf-evidence bead can be reopened (or a new
sibling filed) with real allocation deltas and p95/p99 latency
measurements on the fixtures listed above. The expected wins are:

- Allocation count delta: roughly proportional to the per-request
  `with_capacity(candidate_count)` allocations avoided by reuse,
  i.e. one `PackDraftScratch` worth of `Vec` capacity per request
  after the first.
- Latency delta: dominated by `Vec` allocation amortization, expected
  on the order of microseconds per request for moderate fixtures —
  meaningful only as a p99 tail metric on graph-heavy packs.

## Verification posture

The perf-evidence verification clause in bd-1prrl.7.5 reads:

> Perf/bench evidence runs through the repo-approved RCH path. If RCH
> is blocked with RCH-E104/RCH-E327 or worker pressure, preserve the
> exact blocker in the bead comment and do not run local Cargo
> fallback.

This document is a structural analysis, not a measurement. No
Criterion benchmark is added to the normal readiness gate because the
acceptance prohibits it ("No Criterion benchmark is added to the
normal readiness gate unless it is already part of the explicit
benchmark gate"). A future `workspace_reuse` bead is the natural home
for a new entry in `benches/context.rs` registered to the explicit
bench gate (see `tests/l2_pack_cache_perf_gate.rs` for the
registration pattern: bench fn → `scripts/bench.sh` runner →
`benches/budgets.toml` budgets → `benches/baselines/perf_v0_2.json`
baseline).

The RCH topology state at the time of this writeup
(2026-05-24T21:21:01Z) is: remote worker `vmi1227854` accepts the
build, but `cargo check --tests` fails with 101 pre-existing peer-WIP
errors across `src/cli/mod.rs`, `src/core/curate.rs`, `src/curate/mod.rs`,
`src/db/mod.rs`, `src/config/merge.rs`, `src/core/beads_integrity.rs`,
`src/core/tripwire.rs`, `src/graph/algorithms.rs`,
`src/mesh/foreground_cli.rs`, `src/core/quarantine.rs`, and
`src/runtime/determinism.rs` (E0061, E0063, E0308, E0422, E0425,
E0433, E0597, E0716). None of those files are touched by bd-1prrl.7.x.
A measurement attempt would block on the same upstream rot; the
structural analysis above does not require a working build.

## Conclusion

`ArenaMode::RequestScoped` should remain non-default in
`PackAssemblyOptions` and in `src/core/context.rs`. The arena
scaffolding shipped through bd-1prrl.7.1 – 7.4 is the lifetime and
parity contract that lets a future `workspace_reuse` bead land safely
with real perf evidence. Until that bead lands, there is no allocation
or latency delta to measure between the two currently-supported modes.
