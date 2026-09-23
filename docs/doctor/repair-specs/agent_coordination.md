# agent_coordination repair specs

Repair specs for the `agent_coordination` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-agent_coordination-rch-workers-all-blocked-by-pressure

- **Label:** NOT-DETECTED (pinned gap; NOT coverage)
- **Severity:** P1. Scored P1 as `pop-rch-pressure` in `docs/doctor/failure_mode_scores.jsonl`: blocked build workers stop remote verification only. The manifest said P0 until the bd-2oh15 c9891 ruling (the rubric governs).
- **Detector:** None as a failure. `doctor --full` parses `rchWorkerPressure.status` as `healthy_but_pressure_blocked` with `usableWorkerCount` 0, but the `rch_worker_pressure` check keeps severity `ok`.
- **Real trigger:** The state lives in an external tool. A stand-in `rch` placed first on `PATH` (a double for the external binary, not for ee internals) reports its only worker at critical disk pressure with 0 GB free.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable: nothing is written.
- **Oracle:** `doctor_fixture_assert_pinned_gap`. The witness is the `doctor --full` report above: all workers blocked, check severity `ok`.
- **Negative control:** `doctor_fixture_healthy_store` asserts the store is healthy before the stand-in is placed on `PATH`.
- **Pinned sha:** 9188a0661, measured on the ab83e3f23 release binary.

## fm-agent_coordination-mcp-agent-mail-file-reservation-conflict

- **Label:** NOT-DETECTED (pinned gap; NOT coverage; bd-2oh15 ruling c9986)
- **Severity:** P1. Scored P1 as `pop-agent_mail_snapshot-stale_exclusive_lease`, a row added after the blind inventory (bd-2oh15 c9985/c9986), because no doctor check reads Agent Mail reservations.
- **Detector:** None. `src/core/doctor_fixers.rs` declares the family as FM-AC-01 (`fix_agent_coordination_stale_lease`, finding `agent_coordination_stale_lease`: quarantine the reservation marker), but no doctor check reads the Agent Mail snapshot, so it is never dispatched.
- **Real trigger:** A git checkout with one committed file, `src/leased.rs`, that is then modified, and a workspace snapshot `.ee/agent-mail-snapshot.json`: a declared `ee.agent_mail.snapshot.v1` (six ordered source commands, index-matched statuses, `project_key` bound to the canonical workspace path, no degradation, and the `health_level` `green` / `durability_state` `ok` that the strict validator requires after successful health probes). It lists an EXCLUSIVE reservation on `src/leased.rs` held by another agent, with `expires_ts` `2026-01-01T00:00:00Z`, long past. `ee workspace hygiene` accepts the snapshot and classifies the lease `expired_reservation_ignored`. The fixture needs `git` on `PATH`.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable. The snapshot is unchanged by `--fix`.
- **Oracle:** `doctor_fixture_assert_pinned_gap` on `.ee/agent-mail-snapshot.json`: doctor healthy with an empty `actionable`, `--fix` reports 0 actions, and the snapshot bytes are unchanged. The witness is `ee workspace hygiene --agent-mail-snapshot`: `coordinationState.agentMailAvailable` is true, nothing is blocked, and `ignoredReservations` holds the exclusive lease on `src/leased.rs` with reason `expired_reservation_ignored`. Hygiene trusts only a snapshot generated in the last few minutes, so the witness reads a copy under `.fixture_baseline/` whose `generated_at` is re-stamped; the damaged snapshot itself is never rewritten.
- **Negative control:** The same lease with a future `expires_ts` is live: hygiene blocks `src/leased.rs` with `active_exclusive_reservation` (`corrupt.sh`), which proves the snapshot is accepted and the pattern matches. A measured control that swaps in that live snapshot makes `assert.sh` fail with "witness lost".
- **Pinned sha:** 25a9d7f8d, measured on its stamped release build on vmi1227854 (corrupt 0, assert 0: "pinned gap confirmed"; the live-lease control failed with "witness lost"). A first version without `health_level` and `durability_state` was rejected by the validator on 02524267e.
