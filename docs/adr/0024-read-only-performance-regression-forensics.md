# ADR 0024: Read-Only Performance Regression Forensics

Status: proposed
Date: 2026-05-08

## Context

ADR 0017 and the `eidetic_engine_cli-fcq1` track define the swarm-scale
resource-governance foundation: deterministic workload fixtures, RCH-friendly
benchmark reports, cache reports, write-queue reports, explain-performance
reports, swarm-contention E2E artifacts, and support-bundle scale evidence.

ADR 0023 and the `eidetic_engine_cli-k8dp` track define the host-adaptive
profile foundation: redaction-safe host probes, profile reports, selected
budgets, profile evidence in support bundles, and RCH-friendly verification
recipes for constrained, portable, workstation, and swarm hosts.

The next gap is comparison, not another profiler, scheduler, benchmark harness,
or host-profile system. Operators and agents need a deterministic read-only
surface that compares two already-produced artifacts and answers:

1. What changed?
2. How severe is the change?
3. Which subsystem probably owns the change?
4. Which evidence supports or weakens that conclusion?
5. What command should an agent run next?

The comparison surface must consume existing reports and bundles from the fcq1
and k8dp foundations. It must not duplicate their collection logic or mutate
budgets, caches, indexes, database rows, profile selection, support bundles, or
Beads state.

## Decision

`ee` will add a read-only differential performance-forensics contract. Compare
and budget-check commands read normalized performance artifacts and emit stable
JSON on stdout. Human diagnostics, progress, and repair text remain on stderr.
The commands do not write memory records, support bundles, indexes, caches,
profile config, Beads data, or artifact files.

The initial artifact classes are:

- `benchmark_report`: RCH-friendly benchmark or smoke summary from the
  swarm-scale track. It records command profile, fixture tier, elapsed timing,
  RSS or memory estimate when available, output hash, stderr posture, and
  budget metadata.
- `support_bundle_manifest`: Redacted support-bundle manifest with file list,
  artifact kinds, content hashes, redaction summary, schema versions, and
  profile evidence references.
- `explain_performance_report`: Redaction-safe report from `context`, `pack`,
  or `search` explain mode. It records candidate counts, cache state,
  DB/index generations, pruning stages, timings, degradations, and selected
  profile evidence when available.
- `profile_evidence`: Host probe, selected profile, effective budgets,
  override provenance, degraded profile codes, and verification recipe from
  ADR 0023.
- `cache_report`: Derived-cache report with generation, hotset/prewarm state,
  hit/miss counters when available, memory budget, stale/fallback posture, and
  redaction posture.
- `write_queue_report`: Single-write-owner or write-spool report with queue
  depth, batch state, coalescing window, retry/backpressure posture, audit
  generation, cancellation posture, and recovery state.
- `swarm_contention_report`: Logged multi-process E2E or smoke report with
  per-command pid, start/end time, exit code, stderr posture, stdout hash,
  artifact path, contention scenario, and recovery result.

Each artifact summary has a stable envelope before class-specific fields:

- `artifactId`: caller-provided label or deterministic hash.
- `artifactKind`: one of the artifact classes above.
- `schema`: artifact schema name and version.
- `sourcePath`: redacted path label or omitted when unavailable.
- `contentHash`: declared hash when present.
- `observedHash`: hash computed by the reader when the artifact is local.
- `profile`: normalized profile evidence or explicit absence.
- `metrics`: normalized comparable metrics.
- `degraded`: sorted degradation records.
- `redaction`: redaction posture and uncertainty level.

The compare output is deterministic for the same two normalized artifact
summaries, command options, and `ee` version. Ranking ties use stable keys:
severity, subsystem owner, metric name, artifact kind, and artifact ID.

The public JSON output shape is:

```json
{
  "schema": "ee.perf.compare.v1",
  "artifacts": {
    "baseline": {},
    "candidate": {}
  },
  "summary": {
    "result": "regressed",
    "confidence": "medium",
    "worstSeverity": "high"
  },
  "deltas": [],
  "ownerHints": [],
  "degraded": [],
  "nextCommands": []
}
```

The `result` field is one of `improved`, `unchanged`, `regressed`, `mixed`, or
`inconclusive`. An inconclusive result is a successful honest answer when the
inputs are readable but insufficient. A command fails only when required input
cannot be read, parsed, normalized, or policy-approved.

Subsystem owner hints use a fixed taxonomy:

- `retrieval`: query parsing, Frankensearch search, candidate generation, or
  result ranking.
- `packing`: context-pack pruning, token budgeting, provenance assembly, or
  explain output.
- `storage`: FrankenSQLite/SQLModel reads, generation checks, migrations, or
  DB health.
- `indexing`: derived search indexes, rebuild state, stale generations, or
  index cache effects.
- `cache`: hotset prewarm, cache memory budgets, hit/miss behavior, or stale
  cache fallback.
- `write_owner`: single-write-owner, write spool, queue pressure, batching,
  idempotency, or recovery.
- `profile`: host probe, effective profile, budget overrides, or verification
  recipe differences.
- `support`: support-bundle manifest, artifact inclusion, hash verification, or
  redaction behavior.
- `runtime`: Asupersync cancellation, budget, supervision, or command outcome
  propagation.
- `unknown`: evidence does not justify a narrower owner.

Owner hints are explanations, not automatic assignments. They must include the
metric or degradation records that caused the hint.

## Degradation Behavior

The compare surface preserves degraded states instead of silently substituting
zeros, empty metrics, or passing results.

- Missing comparable metrics produce `missing_metric` records and lower
  confidence. They do not count as zero-valued deltas.
- Mismatched profiles produce `profile_mismatch` records. The command may still
  compare invariant metrics, but profile-sensitive deltas become inconclusive
  unless the caller explicitly allows cross-profile comparison.
- Stale schema versions produce `stale_schema_version` records. Known older
  schemas may normalize with lowered confidence; unknown versions produce an
  unsupported-schema error.
- Tampered hashes produce `tampered_hash` records when a declared content hash
  does not match the observed hash. Any metric from that artifact is excluded
  from severity decisions unless the caller explicitly requests inspection-only
  output.
- Unsupported artifact kinds produce `unsupported_artifact_kind` errors before
  comparison. They are not coerced into generic JSON.
- Redaction uncertainty produces `redaction_uncertain` records. Secret-bearing
  or uncertain free text is omitted from stdout, and affected explanation fields
  use redacted labels or repair hints instead.

Degradation records are sorted by severity, code, artifact ID, and field path.
Every record includes a stable code, severity, artifact side, affected field,
message, and repair hint.

## Consequences

Agents get a small operator-facing answer before reaching for flamegraphs or
manual log reading. A future command can compare two support bundles, benchmark
reports, or explain-performance outputs and report whether latency, RSS,
candidate count, cache posture, write pressure, profile selection, or swarm
contention changed in a meaningful way.

The design keeps collection and comparison separate. fcq1 continues to own
scale fixtures, benchmark/support artifacts, cache and write-governance
reports, performance explainability, and contention E2E coverage. k8dp
continues to own host probes, profile reports, budgets, and profile evidence.
This ADR only defines how already-produced artifacts are normalized, compared,
degraded, and explained.

The read-only rule makes the surface safe for support triage and CI checks. A
budget-check command may say that a candidate run exceeded a configured budget,
but it must not update the budget, promote a profile, rewrite artifacts, or
mark a Bead closed.

The design also makes implementation stricter:

- Normalizers need explicit schemas and golden fixtures before compare logic
  depends on them.
- Timing and resource fields must distinguish measured values, unavailable
  values, redacted values, and non-comparable values.
- Support-bundle integration must verify hashes before trusting metrics.
- CLI surfaces must preserve stable stdout JSON even when repair text is useful
  for humans.
- The compare core must stay deterministic even though its inputs may contain
  volatile performance measurements.

## Rejected Alternatives

- **Build a profiler daemon.** External profilers can still be useful, but this
  surface compares existing redaction-safe artifacts and keeps ordinary CLI
  commands one-shot.
- **Replace the fcq1 benchmark harness.** Benchmark collection remains in the
  swarm-scale resource-governance track. The compare surface consumes its
  reports.
- **Replace the k8dp profile system.** Profiles and budget selection remain in
  the host-adaptive profile track. The compare surface only checks and explains
  profile evidence differences.
- **Infer missing metrics as zero.** That produces false regressions and false
  passes. Missing data is a visible degradation.
- **Accept arbitrary JSON as comparable evidence.** Unsupported artifacts must
  fail early so future agents do not build a second untyped metrics stack.
- **Write remediation artifacts automatically.** Follow-up commands may be
  suggested, but comparison is read-only.
- **Use custom retrieval, graph, or storage stacks.** This surface does not
  change Frankensearch, FrankenNetworkX, FrankenSQLite, SQLModel, or Asupersync
  ownership.

## Verification

The decision remains true when the perf-forensics track proves all of the
following:

1. `eidetic_engine_cli-mwjq.2` defines normalized artifact schemas and fixtures
   for benchmark reports, support-bundle manifests, explain-performance
   reports, profile evidence, cache reports, write-queue reports, and
   swarm-contention reports.
2. `eidetic_engine_cli-mwjq.3` implements a deterministic compare core with
   stable severity ordering, owner hints, confidence calculation, and degraded
   behavior for missing metrics, profile mismatch, stale schemas, tampered
   hashes, unsupported artifact kinds, and redaction uncertainty.
3. `eidetic_engine_cli-mwjq.4` exposes read-only CLI surfaces with stable JSON
   stdout, stderr-only human diagnostics, no artifact mutation, and explicit
   exit-code behavior.
4. `eidetic_engine_cli-mwjq.5` freezes golden outputs for unchanged, improved,
   regressed, mixed, inconclusive, and degraded comparisons.
5. `eidetic_engine_cli-mwjq.6` adds logged no-mock E2E coverage using RCH-safe
   verification, explicit `CARGO_TARGET_DIR`, stdout hashes, stderr capture,
   and artifact paths.
6. `eidetic_engine_cli-mwjq.7` verifies support-bundle integration: manifest
   discovery, class mapping, hash validation, redaction posture, and
   inspection-only behavior for tampered artifacts.
7. `eidetic_engine_cli-mwjq.8` documents the operator workflow for triaging a
   regression from compare output to the next command.
8. `eidetic_engine_cli-mwjq.9` verifies that profile budgets and profile
   evidence from k8dp are compared against observed artifacts without silently
   crossing incompatible profiles.
9. Forbidden-dependency audits continue to reject Tokio, rusqlite, petgraph,
   and other banned crates.
10. Every implementation bead that runs Cargo verification uses `rch exec --`
    with an explicit `CARGO_TARGET_DIR`.
11. `bv --robot-insights` reports no dependency cycles for the
    `eidetic_engine_cli-mwjq` planning track before the epic closes.
