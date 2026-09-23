# fm-schema_migrations-pending-migrations-detected

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-schema_migrations-pending-migrations-detected` |
| Severity | P1 |
| Subsystem | schema_migrations |
| Repair spec | [`docs/doctor/repair-specs/schema_migrations.md#fm-schema_migrations-pending-migrations-detected`](../../../docs/doctor/repair-specs/schema_migrations.md#fm-schema_migrations-pending-migrations-detected) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-schema_migrations-pending-migrations-detected.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). The damage is real:
   the newest row of the migration ledger is gone, so `ee doctor` lists
   `database` `EE-E700`. It then runs `doctor_fixture_assert_guidance_only` in
   `lib.sh`: an unscoped `ee doctor --fix` exits 6 `completed_partial` with
   `fixerDispatchPending`, every fixer result is `run_migration`
   `guidance_recorded`, `EE-E700` is still reported afterwards, and the
   workspace content digest and the ledger row count are unchanged. No undo
   step runs because nothing is written.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **GUIDANCE-ONLY** (repair spec and `manifest.json`). Doctor detects
the pending migration as `EE-E700`, and `ee doctor --fix` records
`run_migration` guidance without applying it: nothing is repaired.
There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with
`EE_DOCTOR_FIXTURE_RUN_EE=1` when an `ee` binary is on `PATH`; its caller, the
`ee doctor Safety Harness` stage of `scripts/verify.sh`, is not run by any CI
workflow (bd-feftl).
