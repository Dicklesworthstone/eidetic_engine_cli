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
   `EE_DOCTOR_FIXTURE_BINARY`, `doctor_fixture_assert` in `lib.sh`
   additionally runs an unscoped `ee doctor --fix`, a follow-up `ee doctor`
   report (the `--only` it passes filters nothing without `--fix`), then
   `ee doctor --undo <runId>`, and finally compares the post-undo SHA-256
   manifest against the pre-fix baseline (round-trip byte-identical). The
   marker is not real damage, so this round trip exercises undo only.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **UNRESOLVED** (repair spec and `manifest.json`). A real trigger has
been attempted and does not yet produce the failure, so this fixture is
marker-only and is not detector or repair coverage for
`fm-state_files-sqlite-wal-shm-sidecar-drift`. There is no per-FM fix:
`ee doctor --fix --only <id>` is a usage error, because `--fix` declares a
conflict with `--only`. `scripts/verify-undo.sh` runs the round trip above with
`EE_DOCTOR_FIXTURE_RUN_EE=1` when an `ee` binary is on `PATH`; its caller, the
`ee doctor Safety Harness` stage of `scripts/verify.sh`, is not run by any CI
workflow (bd-feftl).
