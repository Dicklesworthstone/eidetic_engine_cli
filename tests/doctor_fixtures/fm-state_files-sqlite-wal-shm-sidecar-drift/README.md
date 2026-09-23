# fm-state_files-sqlite-wal-shm-sidecar-drift

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-state_files-sqlite-wal-shm-sidecar-drift` |
| Severity | P0 |
| Subsystem | state_files |
| Repair spec | [`docs/doctor/repair-specs/state_files.md#fm-state_files-sqlite-wal-shm-sidecar-drift`](../../../docs/doctor/repair-specs/state_files.md#fm-state_files-sqlite-wal-shm-sidecar-drift) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-state_files-sqlite-wal-shm-sidecar-drift.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` confirms the marker is present. When
   `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary is provided in
   `EE_DOCTOR_FIXTURE_BINARY`, it additionally runs
   `ee doctor --fix --only fm-state_files-sqlite-wal-shm-sidecar-drift`, then a follow-up
   `ee doctor` read-back, then `ee doctor undo --last`,
   and finally compares the post-undo SHA-256 manifest
   against the pre-fix baseline (round-trip byte-identical).

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

`ee doctor --fix --only fm-state_files-sqlite-wal-shm-sidecar-drift` is WIRED. `bd-3boan` (CLI surface for
the doctor runtime) is closed and `DoctorArgs` carries both `--fix` and
`--only`, so `scripts/verify-undo.sh` sets `EE_DOCTOR_FIXTURE_RUN_EE=1` and
the round-trip above runs under the `ee doctor Safety Harness` stage of
`scripts/verify.sh`.
