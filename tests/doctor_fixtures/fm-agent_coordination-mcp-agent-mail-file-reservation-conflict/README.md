# fm-agent_coordination-mcp-agent-mail-file-reservation-conflict

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-agent_coordination-mcp-agent-mail-file-reservation-conflict` |
| Severity | P1 |
| Subsystem | agent_coordination |
| Repair spec | [`docs/doctor/repair-specs/agent_coordination.md#fm-agent_coordination-mcp-agent-mail-file-reservation-conflict`](../../../docs/doctor/repair-specs/agent_coordination.md#fm-agent_coordination-mcp-agent-mail-file-reservation-conflict) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-agent_coordination-mcp-agent-mail-file-reservation-conflict.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). Its independent
   witness is `ee workspace hygiene --agent-mail-snapshot`: the workspace's
   exclusive lease on the dirty `src/leased.rs` must read as
   `expired_reservation_ignored`, with Agent Mail available and nothing
   blocked. Hygiene trusts only a recently generated snapshot, so the witness
   reads a copy under `.fixture_baseline/` with a fresh `generated_at`; the
   damaged `.ee/agent-mail-snapshot.json` is never rewritten. It then runs
   `doctor_fixture_assert_pinned_gap` in `lib.sh`: `ee doctor` must report the
   workspace healthy, an unscoped `ee doctor --fix` must take 0 actions, and
   the snapshot must be byte-identical afterwards. No undo step runs because
   nothing is written. `corrupt.sh` needs `git` on `PATH`.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **NOT-DETECTED** (pinned gap; NOT coverage; repair spec and
`manifest.json`). Doctor reads no Agent Mail snapshot, so `ee doctor --fix`
dispatches nothing for the stale lease, although
`fix_agent_coordination_stale_lease` (FM-AC-01) exists. A passing run means
the gap is still there; when a detector lands the fixture goes red on purpose
and is relabelled.
There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with `EE_DOCTOR_FIXTURE_RUN_EE=1`
when an `ee` binary is on `PATH`; its caller, the `ee doctor Safety Harness`
stage of `scripts/verify.sh`, is not run by any CI workflow (bd-feftl).
