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
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). `ee doctor` must
   report posture `blocked` with `database` and `search_index` both
   `EE-E202`. It then runs `doctor_fixture_assert_guidance_only` in `lib.sh`:
   an unscoped `ee doctor --fix` exits 6 `completed_partial` with
   `fixerDispatchPending`, every fixer result is `manual`
   `guidance_recorded` (`database_corrupted`), `EE-E202` is still reported
   afterwards, no index rebuild request is written, and the workspace content
   digest is unchanged. No undo step runs because nothing is written.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **GUIDANCE-ONLY** (repair spec and `manifest.json`). Doctor detects
the damage as `EE-E202`, and since bd-xa6ud closed `ee doctor --fix` records
`database_corrupted` guidance instead of dispatching an index repair against a
database it cannot open: nothing is repaired.
There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with `EE_DOCTOR_FIXTURE_RUN_EE=1`
when an `ee` binary is on `PATH`; its caller, the `ee doctor Safety Harness`
stage of `scripts/verify.sh`, is not run by any CI workflow (bd-feftl).
