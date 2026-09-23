# fm-search_indexes-index_stale

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-search_indexes-index_stale` |
| Severity | P1 |
| Subsystem | search_indexes |
| Repair spec | [`docs/doctor/repair-specs/search_indexes.md#fm-search_indexes-index_stale`](../../../docs/doctor/repair-specs/search_indexes.md#fm-search_indexes-index_stale) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-search_indexes-index_stale.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). The damage is real:
   the store's workspace generation is one past the generation the index was
   built from, and doctor reports `search_index` `EE-E301`. `doctor_fixture_assert`
   in `lib.sh` runs an unscoped `ee doctor --fix`, which must end
   `completed_ok` with `search_index_stale` / `run_index_rebuild` `applied`
   (never guidance), then `ee doctor --undo <runId>`, which must be `undone`
   with the index generation and `EE-E301` back. A second undo must undo 0
   actions, and the content digest must equal the baseline after both.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **REPAIR** (repair spec and `manifest.json`). Doctor reports the
staleness as `EE-E301` and `ee doctor --fix` repairs it with
`run_index_rebuild`. The fixture runs `--fix` unscoped, the only form there is:
`ee doctor --fix --only <id>` is a usage error, because `--fix` declares a
conflict with `--only`. `scripts/verify-undo.sh` runs the round trip above with
`EE_DOCTOR_FIXTURE_RUN_EE=1` when an `ee` binary is on `PATH`; its caller, the
`ee doctor Safety Harness` stage of `scripts/verify.sh`, is not run by any CI
workflow (bd-feftl).
