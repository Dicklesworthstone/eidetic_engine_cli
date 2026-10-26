# graph_subsystem repair specs

Repair specs for the `graph_subsystem` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## Current snapshot inspection and recovery

The advisory `graph_snapshot` check inspects the addressed workspace's latest
`memory_links` snapshot and source links in one read transaction. It checks the
stored status and content hash, additions and deletions, and exact selected
edges (including endpoints, weight, confidence, evidence count, and projected
provenance). A failed source query reports `EE-E207` (unmeasured), rather than
claiming that a refresh will repair an unreadable database.

New `ee.graph.snapshot.metrics.v2` snapshots record `sourceProjection`, so the
check replays the original link limit, minimum weight/confidence, and mesh
visibility policy. Legacy snapshots lack that selection record; their byte
integrity remains checkable, but freshness is explicitly unverified until
refreshed. Other typed graph snapshots keep their existing v1 topology format
and are outside this check's scope.

Missing populated projections, stale or invalid snapshots, and unverified
legacy snapshots emit `EE-E304`. This code maps only the `graph_snapshot` check
to `graph_snapshot_stale`. `ee doctor --fix` records **AutoGuidance**, never a
completed graph repair: run the supplied workspace-specific
`ee graph centrality-refresh --workspace <path> --json`, then inspect
`ee doctor --workspace <path> --json`. The refresh command performs the existing
transactional snapshot publication, archive audit, and algorithm-cache
invalidation. If the final source link was removed, refresh publishes an empty
replacement, clearing the old scores; an initially empty graph needs no
snapshot. Dry runs do not publish replacements.

Regression coverage lives in `src/graph/mod.rs`, `src/core/doctor.rs`,
`src/core/doctor_fixers.rs`, and `src/core/graph_diff.rs`. The historical shell
fixtures below have not been upgraded or measured by this change; these code
changes do not count as a pinned fixture execution or database undo coverage.

## fm-graph_subsystem-snapshot-stale

- **Label:** UNCLASSIFIED
- **Severity:** P1. Scored P1 as `pop-graph_snapshot-stale` in `docs/doctor/failure_mode_scores.jsonl`.
- **Detector:** Implemented by `graph_snapshot` (`EE-E304`), as described above. This shell fixture remains unmeasured.
- **Real trigger:** Not yet built.
- **Repair:** AutoGuidance for the audited `graph centrality-refresh` command; no automatic database undo is claimed.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.

## fm-graph_subsystem-snapshot-missing

- **Label:** UNCLASSIFIED
- **Severity:** P1. Scored P1 as `pop-graph_snapshot-missing`.
- **Detector:** Implemented by `graph_snapshot` (`EE-E304`) when visible source links exist. An empty source without a snapshot is healthy. This shell fixture remains unmeasured.
- **Real trigger:** Not yet built.
- **Repair:** AutoGuidance for the audited `graph centrality-refresh` command.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.

## fm-graph_subsystem-snapshot-write-lock-held

- **Label:** NOT-DETECTED (pinned gap; NOT coverage)
- **Severity:** P1. Scored P1 as `pop-graph_lock-held`: a held lease blocks graph refresh only and leaves memory data untouched. The manifest said P0 until the bd-2oh15 c9891 ruling (the rubric governs).
- **Detector:** None. Doctor never reads `ee_advisory_locks`.
- **Real trigger:** SQL inserts a row into `ee_advisory_locks` with resource key `graph_snapshot:<workspace id>:memory_links`, holder `ee-graph-snapshot-1-fixture` (not PID-reclaimable) and an expiry in 2099. A byte copy of the pre-lock database is kept in `.fixture_baseline/ee.db.pre-lock`.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable. The lock row still exists after `--fix`.
- **Oracle:** `doctor_fixture_assert_pinned_gap`. The witness is `ee graph centrality-refresh`, which exits 4 with `held by ee-graph-snapshot-1-fixture`.
- **Negative control:** `doctor_fixture_healthy_store` asserts the store is healthy before the insert.
- **Pinned sha:** 9188a0661, measured on the ab83e3f23 release binary.
