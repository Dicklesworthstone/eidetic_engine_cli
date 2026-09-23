# fm-search_indexes-index_corrupt

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-search_indexes-index_corrupt` |
| Severity | P1 |
| Subsystem | search_indexes |
| Repair spec | [`docs/doctor/repair-specs/search_indexes.md#fm-search_indexes-index_corrupt`](../../../docs/doctor/repair-specs/search_indexes.md#fm-search_indexes-index_corrupt) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-search_indexes-index_corrupt.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). Through
   `doctor_fixture_assert` in `lib.sh` it runs an unscoped `ee doctor --fix`, a
   follow-up `ee doctor` report, then `ee doctor --undo <runId>`, and compares
   the post-undo SHA-256 manifest against the pre-fix baseline. It then
   asserts the fix applied `search_index_stale` (`run_index_rebuild`) with
   status `completed_ok`, that undo brought the corrupted file and `EE-E301`
   back, and that a second undo is a no-op.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **REPAIR** (repair spec and `manifest.json`). Doctor reports the
corruption as `EE-E301` and `ee doctor --fix` repairs it with
`fix_search_index_stale`. The fixture runs `--fix` unscoped, the only form there is:
`ee doctor --fix --only <id>` is a usage error, because `--fix` declares a
conflict with `--only`. `scripts/verify-undo.sh` runs this fixture with
`EE_DOCTOR_FIXTURE_RUN_EE=1` when an `ee` binary is on `PATH`; its caller, the
`ee doctor Safety Harness` stage of `scripts/verify.sh`, is not run by any CI
workflow (bd-feftl).
