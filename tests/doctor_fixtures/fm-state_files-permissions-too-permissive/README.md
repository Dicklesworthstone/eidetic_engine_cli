# fm-state_files-permissions-too-permissive

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-state_files-permissions-too-permissive` |
| Severity | P1 |
| Subsystem | state_files |
| Repair spec | [`doctor_workspace/analysis/repair_specs/state_files.md`](../../../doctor_workspace/analysis/repair_specs/state_files.md) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-state_files-permissions-too-permissive.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` confirms the marker is present. When
   `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary is provided in
   `EE_DOCTOR_FIXTURE_BINARY`, it additionally runs
   `ee doctor --fix --only fm-state_files-permissions-too-permissive`, then a follow-up
   `ee doctor` read-back, then `ee doctor undo --last`,
   and finally compares the post-undo SHA-256 manifest
   against the pre-fix baseline (round-trip byte-identical).

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

`ee doctor --fix --only fm-state_files-permissions-too-permissive` is currently gated by
`bd-3boan` (CLI surface for the doctor runtime); set
`EE_DOCTOR_FIXTURE_RUN_EE=1` only once the CLI wiring lands.
