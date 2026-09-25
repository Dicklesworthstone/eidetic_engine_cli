# fm-schema_migrations-migration-drift-checksum-mismatch

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-schema_migrations-migration-drift-checksum-mismatch` |
| Severity | P0 |
| Subsystem | schema_migrations |
| Repair spec | [`docs/doctor/repair-specs/schema_migrations.md#fm-schema_migrations-migration-drift-checksum-mismatch`](../../../docs/doctor/repair-specs/schema_migrations.md#fm-schema_migrations-migration-drift-checksum-mismatch) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-schema_migrations-migration-drift-checksum-mismatch.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). `ee doctor` must
   list check `database` with `EE-E702` (migration drift) as actionable. It
   then runs `doctor_fixture_assert_guidance_only` in `lib.sh`: an unscoped
   `ee doctor --fix` exits 6 recording only `database_migration_drift` manual
   guidance (never `database_corrupted`), `EE-E702` and the `migration_drift`
   message are still reported afterwards, the content digest is unchanged, and
   the tampered checksum is still stored. No undo step runs because nothing is
   written.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **GUIDANCE-ONLY** (repair spec and `manifest.json`). Doctor detects
this failure as `EE-E702` (migration drift) with posture `blocked`, and
`ee doctor --fix` records `database_migration_drift` guidance (reconcile the
binary and the applied history) without writing: nothing is repaired, which is
correct for an intact store. Before bd-ixxzq it was `EE-E202` with the
corrupted-store plan. There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with `EE_DOCTOR_FIXTURE_RUN_EE=1`
when an `ee` binary is on `PATH`; its caller, the `ee doctor Safety Harness`
stage of `scripts/verify.sh`, is not run by any CI workflow (bd-feftl).
