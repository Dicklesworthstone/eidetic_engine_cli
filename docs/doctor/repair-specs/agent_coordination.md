# agent_coordination repair specs

Repair specs for the `agent_coordination` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-agent_coordination-rch-workers-all-blocked-by-pressure

- **Label:** NOT-DETECTED (pinned gap; NOT coverage)
- **Severity:** P0. Scored P1 as `pop-rch-pressure` in `docs/doctor/failure_mode_scores.jsonl`: blocked build workers stop remote verification only.
- **Detector:** None as a failure. `doctor --full` parses `rchWorkerPressure.status` as `healthy_but_pressure_blocked` with `usableWorkerCount` 0, but the `rch_worker_pressure` check keeps severity `ok`.
- **Real trigger:** The state lives in an external tool. A stand-in `rch` placed first on `PATH` (a double for the external binary, not for ee internals) reports its only worker at critical disk pressure with 0 GB free.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable: nothing is written.
- **Oracle:** `doctor_fixture_assert_pinned_gap`. The witness is the `doctor --full` report above: all workers blocked, check severity `ok`.
- **Negative control:** `doctor_fixture_healthy_store` asserts the store is healthy before the stand-in is placed on `PATH`.
- **Pinned sha:** 9188a0661, measured on the ab83e3f23 release binary.

## fm-agent_coordination-mcp-agent-mail-file-reservation-conflict

- **Label:** UNCLASSIFIED
- **Severity:** P1. Not in the scored population: Agent Mail reservations are not read by any doctor check.
- **Detector:** Not measured. The fixture is marker-only.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.
