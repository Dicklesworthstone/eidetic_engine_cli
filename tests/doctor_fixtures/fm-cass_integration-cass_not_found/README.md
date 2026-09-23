# fm-cass_integration-cass_not_found

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-cass_integration-cass_not_found` |
| Severity | P1 |
| Subsystem | cass_integration |
| Repair spec | [`docs/doctor/repair-specs/cass_integration.md#fm-cass_integration-cass_not_found`](../../../docs/doctor/repair-specs/cass_integration.md#fm-cass_integration-cass_not_found) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-cass_integration-cass_not_found.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). It runs
   `doctor_fixture_assert_report_only` in `lib.sh`: `ee doctor --full` must
   report check `cass` with `EE-E506` before and after an unscoped
   `ee doctor --fix` that takes 0 actions and records no fixer result. No undo
   step runs because nothing is written.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **GUIDANCE-ONLY** (report-only; repair spec and `manifest.json`).
Doctor detects this failure as `EE-E506` (in `ee doctor --full` only), but
`ee doctor --fix` has no dispatch for `EE-E506` and makes no write: nothing is
repaired. There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with `EE_DOCTOR_FIXTURE_RUN_EE=1`
when an `ee` binary is on `PATH`; its caller, the `ee doctor Safety Harness`
stage of `scripts/verify.sh`, is not run by any CI workflow (bd-feftl).
