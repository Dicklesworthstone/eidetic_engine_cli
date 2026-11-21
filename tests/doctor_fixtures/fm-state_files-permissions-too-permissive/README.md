# fm-state_files-permissions-too-permissive

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-state_files-permissions-too-permissive` |
| Severity | P1 |
| Subsystem | state_files |
| Repair spec | [`docs/doctor/repair-specs/state_files.md#fm-state_files-permissions-too-permissive`](../../../docs/doctor/repair-specs/state_files.md#fm-state_files-permissions-too-permissive) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-state_files-permissions-too-permissive.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` (it exits 2 without
   it). Its independent witness is the mode string from `ls -ld`: `.ee` and
   `.ee/ee.db` start at 0755 and 0644. Read-only `ee doctor` must report
   `state_directory_permissions` and `state_file_permissions` with EE-E208
   while preserving the bytes and modes. An unscoped `ee doctor --fix`
   must apply both `chmod` repairs, resulting in 0700 and 0600. The database
   bytes must remain unchanged, and a second fix must dispatch neither
   permission repair. Undo restores the exact original 0755 and 0644 modes
   and verifies the complete content digest.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **REPAIR** (executable repair contract; RCH runtime verification is
pending for this implementation). The detector and fixed-path dispatch now
reach `TightenPermissions`, recorded as the existing reversible `chmod` action.
No permission bits are added, so deliberately more restrictive owner modes
survive repair. Symlinks and unexpected file types receive EE-E209 and no
automatic permission fixer. Database access failures remain independently
unresolved; changing permissions does not claim to restore missing or damaged data.
There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with `EE_DOCTOR_FIXTURE_RUN_EE=1`
when an `ee` binary is on `PATH`; its caller, the `ee doctor Safety Harness`
stage of `scripts/verify.sh`, is not run by any CI workflow (bd-feftl).
