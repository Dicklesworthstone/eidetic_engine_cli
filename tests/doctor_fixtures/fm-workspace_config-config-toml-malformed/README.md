# fm-workspace_config-config-toml-malformed

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-workspace_config-config-toml-malformed` |
| Severity | P1 |
| Subsystem | workspace_config |
| Repair spec | [`docs/doctor/repair-specs/workspace_config.md#fm-workspace_config-config-toml-malformed`](../../../docs/doctor/repair-specs/workspace_config.md#fm-workspace_config-config-toml-malformed) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-workspace_config-config-toml-malformed.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` confirms the marker is present. When
   `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary is provided in
   `EE_DOCTOR_FIXTURE_BINARY`, it additionally runs
   `ee doctor --fix --only fm-workspace_config-config-toml-malformed`, then a follow-up
   `ee doctor` read-back, then `ee doctor undo --last`,
   and finally compares the post-undo SHA-256 manifest
   against the pre-fix baseline (round-trip byte-identical).

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

`ee doctor --fix --only fm-workspace_config-config-toml-malformed` is WIRED. `bd-3boan` (CLI surface for
the doctor runtime) is closed and `DoctorArgs` carries both `--fix` and
`--only`, so `scripts/verify-undo.sh` sets `EE_DOCTOR_FIXTURE_RUN_EE=1` and
the round-trip above runs under the `ee doctor Safety Harness` stage of
`scripts/verify.sh`.
