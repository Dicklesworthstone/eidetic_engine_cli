# graph_subsystem repair specs

Repair specs for the `graph_subsystem` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-graph_subsystem-snapshot-stale

- **Label:** UNCLASSIFIED
- **Severity:** P1. Scored P1 as `pop-graph_snapshot-stale` in `docs/doctor/failure_mode_scores.jsonl`.
- **Detector:** Not measured. The inventory found no doctor check that reads `graph_snapshots`.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.

## fm-graph_subsystem-snapshot-missing

- **Label:** UNCLASSIFIED
- **Severity:** P1. Scored P1 as `pop-graph_snapshot-missing`.
- **Detector:** Not measured. The inventory found no doctor check that reads `graph_snapshots`.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.

## fm-graph_subsystem-snapshot-write-lock-held

- **Label:** NOT-DETECTED (pinned gap; NOT coverage)
- **Severity:** P0. Scored P1 as `pop-graph_lock-held`: a held lease blocks graph refresh only and leaves memory data untouched.
- **Detector:** None. Doctor never reads `ee_advisory_locks`.
- **Real trigger:** SQL inserts a row into `ee_advisory_locks` with resource key `graph_snapshot:<workspace id>:memory_links`, holder `ee-graph-snapshot-1-fixture` (not PID-reclaimable) and an expiry in 2099. A byte copy of the pre-lock database is kept in `.fixture_baseline/ee.db.pre-lock`.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable. The lock row still exists after `--fix`.
- **Oracle:** `doctor_fixture_assert_pinned_gap`. The witness is `ee graph centrality-refresh`, which exits 4 with `held by ee-graph-snapshot-1-fixture`.
- **Negative control:** `doctor_fixture_healthy_store` asserts the store is healthy before the insert.
- **Pinned sha:** 9188a0661, measured on the ab83e3f23 release binary.
