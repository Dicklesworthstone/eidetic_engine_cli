# fm-state_files-empty-or-truncated-database

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-state_files-empty-or-truncated-database` |
| Severity | P0 |
| Subsystem | state_files |
| Repair spec | [`docs/doctor/repair-specs/state_files.md#fm-state_files-empty-or-truncated-database`](../../../docs/doctor/repair-specs/state_files.md#fm-state_files-empty-or-truncated-database) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-state_files-empty-or-truncated-database.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). It pins defect
   bd-xa6ud as it is today: `ee doctor` reports posture `blocked` with
   `database` `EE-E202` and `search_index` `EE-E300`, and an unscoped
   `ee doctor --fix` exits 3 with `doctor_runtime_io` ("build doctor index
   repair") while leaving `.ee/ee.db` and the workspace content digest
   unchanged. No undo step runs.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **PINNED-DEFECT bd-xa6ud** (NOT coverage; repair spec and
`manifest.json`). Doctor detects the damage, but `ee doctor --fix` dispatches
the `EE-E300` index repair against a database it cannot open and fails. A
passing run means the defect is still there; when bd-xa6ud is fixed the fixture
goes red on purpose and is relabelled (the target oracle is in the spec).
There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with `EE_DOCTOR_FIXTURE_RUN_EE=1`
when an `ee` binary is on `PATH`; its caller, the `ee doctor Safety Harness`
stage of `scripts/verify.sh`, is not run by any CI workflow (bd-feftl).
