# schema_migrations repair specs

Repair specs for the `schema_migrations` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-schema_migrations-pending-migrations-detected

- **Label:** GUIDANCE-ONLY
- **Severity:** P1. Scored P1 as `pop-migrations-pending` in `docs/doctor/failure_mode_scores.jsonl`.
- **Detector:** `database` reports EE-E700 (a compiled migration is absent from `ee_schema_migrations`); posture `degraded_recoverable`.
- **Real trigger:** SQL removes the newest row of `ee_schema_migrations` from a healthy store (the ledger drops by exactly one row). A byte copy of the database from before the edit is kept in `.fixture_baseline/ee.db.pre-edit`; doctor's lock is provisioned by a healthy no-op `--fix` first.
- **Repair:** None applied. `fix_schema_migration_pending` dispatches the advisory `run_migration` operation, so `--fix` exits 6 with status `completed_partial` and records only `guidance_recorded` for `schema_migration_pending`. No migration runs.
- **Undo:** Not applicable: nothing is written. The content digest is identical around `--fix`, and the ledger is still one row short afterwards.
- **Oracle:** `doctor_fixture_assert_guidance_only` for `schema_migration_pending` / `database` EE-E700 (exit 6, `completed_partial`, `fixerDispatchPending`), plus every fixer result `run_migration` / `guidance_recorded`, an identical content digest around `--fix`, and the ledger row count unchanged.
- **Negative control:** `doctor_fixture_healthy_store` asserts the store is healthy before the edit, and the lock-provisioning `--fix` reports `actionCount` 0.
- **Pinned sha:** 02524267e, measured on its stamped release build on hz3.

## fm-schema_migrations-migration-drift-checksum-mismatch

- **Label:** GUIDANCE-ONLY
- **Severity:** P0. Scored P0 as `pop-migrations-checksum_drift`.
- **Detector:** `database` reports EE-E702 (migration drift) with the EE-E040 `migration_drift` message, and the posture is `blocked`. The route comes from the typed `DbError::MigrationDrift` (its error id is EE-E040), not from the message text. History: EE-E202 with corruption guidance before bd-ixxzq; EE-E207 (unavailable) under the first, prefix-keyed fix 29168c8d8, whose expected prose never matched this message.
- **Real trigger:** SQL sets the stored checksum of the lowest applied migration in `ee_schema_migrations` to `blake3:` followed by 64 zeros. A byte copy of the pre-drift database is kept in `.fixture_baseline/ee.db.pre-drift`.
- **Repair:** None, correctly. `--fix` records one guidance-only fixer for the EE-E702 store (finding `database_migration_drift`, operation `manual`, outcome `guidance_recorded`): reconcile the binary and the applied migration history before changing the database. It no longer names corruption; the cause is migration drift and the store is intact.
- **Undo:** Not applicable: nothing is written. The tampered checksum is still present after `--fix`.
- **Oracle:** `doctor_fixture_assert_guidance_only` for `database_migration_drift` / `database` EE-E702, plus: posture `blocked` before and after, every fixer result manual guidance and none `database_corrupted`, the `migration_drift` message still reported, an identical content digest around `--fix`, and the tampered checksum still stored.
- **Negative control:** `doctor_fixture_healthy_store` asserts the undamaged store is healthy before the update.
- **Pinned sha:** __PIN_DRIFT__

## fm-schema_migrations-shard-fanout-catalog-hash-mismatch

- **Label:** NOT-DETECTED (pinned gap; NOT coverage)
- **Severity:** P0. Scored P0 as `pop-shard-content_drift`.
- **Detector:** None. The catalog's `last_verified_hashes` is never computed or compared, so the `shard_fanout` check stays `ok` in `doctor --full`.
- **Real trigger:** A real `ee migrate shard-fanout --shards-dir <root>/shards` with `EE_SHARD_FANOUT_ENABLED=1` writes `catalog.db` and the workspace shard. SQL then appends text to one row of `memories` in the shard. The migrated shard is kept as `.fixture_baseline/shard.migrated.db`. The flag is passed explicitly because `migrate shard-fanout` ignores an exported `EE_SHARDS_DIR` (bd-qxc0b), while doctor reads it.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable. The harness asserts the shard is unchanged by `--fix`.
- **Oracle:** `doctor_fixture_assert_pinned_gap` on the shard file. The witness: the shard digest differs from `shard.migrated.db` while the catalog row is byte-identical to the one recorded before the tamper, and `shard_fanout` has severity `ok`.
- **Negative control:** `doctor_fixture_healthy_store` asserts the store is healthy before the migration, and `corrupt.sh` fails if the tamper leaves the shard bytes unchanged.
- **Pinned sha:** 9188a0661, measured on the ab83e3f23 release binary.
