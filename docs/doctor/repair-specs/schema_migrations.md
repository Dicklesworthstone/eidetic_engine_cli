# schema_migrations repair specs

Repair specs for the `schema_migrations` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-schema_migrations-pending-migrations-detected

- **Label:** UNCLASSIFIED
- **Severity:** P1. Scored P1 as `pop-migrations-pending` in `docs/doctor/failure_mode_scores.jsonl`.
- **Detector:** Not measured by this fixture. The inventory shows `database` reports EE-E700, which `doctor --fix` dispatches to `fix_schema_migration_pending`.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.

## fm-schema_migrations-migration-drift-checksum-mismatch

- **Label:** GUIDANCE-ONLY
- **Severity:** P0. Scored P0 as `pop-migrations-checksum_drift`.
- **Detector:** `database` reports EE-E202 with an EE-E040 `migration_drift` message, and the posture is `blocked`.
- **Real trigger:** SQL sets the stored checksum of the lowest applied migration in `ee_schema_migrations` to `blake3:` followed by 64 zeros. A byte copy of the pre-drift database is kept in `.fixture_baseline/ee.db.pre-drift`.
- **Repair:** None. Since 9ed78b70d, `--fix` records one guidance-only fixer for the EE-E202 store (finding `database_corrupted`, operation `manual`, outcome `guidance_recorded`); before it, `--fix` reported 0 actions. The guidance names corruption although the cause is migration drift.
- **Undo:** Not applicable: nothing is written. The tampered checksum is still present after `--fix`.
- **Oracle:** `doctor_fixture_assert_guidance_only` for `database_corrupted` / `database` EE-E202, plus: posture `blocked` before and after, every fixer result manual guidance, the `migration_drift` message still reported, an identical content digest around `--fix`, and the tampered checksum still stored.
- **Negative control:** `doctor_fixture_healthy_store` asserts the undamaged store is healthy before the update.
- **Pinned sha:** a2c2f950f, measured on its stamped release build (the report-only oracle held until 9ed78b70d).

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
