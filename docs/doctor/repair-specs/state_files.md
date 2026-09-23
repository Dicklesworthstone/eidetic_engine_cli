# state_files repair specs

Repair specs for the `state_files` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-state_files-sqlite-wal-shm-sidecar-drift

- **Label:** UNRESOLVED
- **Severity:** P0. Scored P0 as `pop-wal_shm-stale_sidecar` in `docs/doctor/failure_mode_scores.jsonl`.
- **Detector:** None known. No doctor check reads `.ee/ee.db-wal` contents or `.ee/ee.db-shm`; `wal_pressure` (EE-E205) compares WAL size only.
- **Real trigger:** Not yet built. Random sidecar bytes produce no failure: SQLite rejects a WAL whose salts do not match the database header. The trigger still to measure is a stale WAL with matching salts, replayed over a newer database file.
- **Repair:** None. No fixer is dispatched for sidecar state.
- **Undo:** Not applicable until a trigger exists.
- **Oracle:** None yet. The fixture is marker-only and does not run ee.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.

## fm-state_files-sqlite-integrity-page-malformed

- **Label:** NOT-DETECTED (pinned gap; NOT coverage)
- **Severity:** P0. Scored P0 as `pop-ee_db-corrupt_page`.
- **Detector:** None. `database` opens the store and reads the migration ledger; it never runs an integrity check, so an interior page can be malformed while doctor reports healthy.
- **Real trigger:** The real `.ee/ee.db` is moved into `.fixture_baseline/` and replaced by a new copy in which the middle 4096-byte page is random bytes. `corrupt.sh` fails unless `sqlite3 PRAGMA integrity_check` stops returning `ok`.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable. The harness asserts the damaged file is unchanged by `--fix`.
- **Oracle:** `doctor_fixture_assert_pinned_gap`: doctor is healthy with an empty `actionable`, `--fix` reports `actionCount` 0 and no `fixerResults`, and the damaged database digest is unchanged. The witness is `PRAGMA integrity_check` != `ok`. This pins the gap; a detector landing turns this fixture red on purpose.
- **Negative control:** `doctor_fixture_healthy_store` asserts the undamaged store is healthy before the damage.
- **Pinned sha:** 9188a0661, measured on the ab83e3f23 release binary.

## fm-state_files-empty-or-truncated-database

- **Label:** GUIDANCE-ONLY (was PINNED-DEFECT bd-xa6ud until 9ed78b70d fixed it)
- **Severity:** P0. Scored P0 as `pop-ee_db-empty_truncated`.
- **Detector:** `database` reports EE-E202 ("the database is truncated: its header records N pages ... but the file holds 8192 bytes") and `search_index` carries the same EE-E202; posture `blocked`. A zero-byte store reports EE-E206 instead (not exercised by this fixture).
- **Real trigger:** The real `.ee/ee.db` is moved into `.fixture_baseline/` and replaced by its own first 8192 bytes. Doctor's lock is provisioned by a healthy no-op `--fix` before the damage.
- **Repair:** None. `--fix` exits 0 with status `completed_ok` and records one guidance-only fixer: finding `database_corrupted`, operation `manual`, outcome `guidance_recorded`. No index or migration repair runs and no index rebuild is requested.
- **Undo:** Not applicable: nothing is written. The `.ee` content digest is identical before and after `--fix`.
- **Oracle:** `doctor_fixture_assert_guidance_only` for `database_corrupted` / `database` EE-E202, plus: EE-E202 on `database` and `search_index` with posture `blocked` before and after `--fix`, every fixer result manual guidance, and an identical content digest around `--fix`. Before 9ed78b70d the fixture pinned bd-xa6ud (exit 3 `doctor_runtime_io`); at 466ee56ee that pin failed as designed, at its detection check (bd-2oh15 c9944).
- **Negative control:** `doctor_fixture_healthy_store` asserts the undamaged store is healthy, and the lock-provisioning `--fix` reports `actionCount` 0.
- **Pinned sha:** 466ee56ee, measured on its stamped release build (fix at 9ed78b70d).

## fm-state_files-merge-conflict-markers

- **Label:** NOT-DETECTED (pinned gap; NOT coverage)
- **Severity:** P1. Scored P1 as `pop-config-malformed`: an unparseable config blocks search and pack until edited, but loses no data. Settled by execution (bd-2oh15 c9891/c9892) on a stamped 11f6e832e build: `status`, `remember` and `doctor` succeed on the conflicted workspace and only `search` fails (exit 2, `configuration`), so the failure does not block every command and P0 clause 2 does not apply. The manifest said P0 until then.
- **Detector:** None. Doctor swallows `config.toml` parse errors.
- **Real trigger:** A valid comment-only `.ee/config.toml` baseline is replaced by the same file wrapped in `<<<<<<<` / `=======` / `>>>>>>>` markers.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable. The harness asserts the config file is unchanged by `--fix`.
- **Oracle:** `doctor_fixture_assert_pinned_gap` on `.ee/config.toml`. The witness is `ee search`, which fails with an `ee.error.v2` envelope whose code is `configuration`.
- **Negative control:** The comment-only baseline config passes `doctor_fixture_healthy_store` before the markers are written.
- **Pinned sha:** 9188a0661, measured on the ab83e3f23 release binary.

## fm-state_files-jsonl-tombstone-drift

- **Label:** UNCLASSIFIED
- **Severity:** P1. Not in the scored population: doctor has no JSONL export check, so this FM is outside the surface the population was built from.
- **Detector:** Not measured. The fixture is marker-only.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.

## fm-state_files-orphaned-pid-write-lock

- **Label:** UNCLASSIFIED
- **Severity:** P1. Scored P1 as `pop-ee_db-locked`.
- **Detector:** Not measured. The inventory shows `database` reports only a lock held past the open timeout, and reports it as EE-E202.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.

## fm-state_files-permissions-too-permissive

- **Label:** UNCLASSIFIED
- **Severity:** P1. Scored P1 as `pop-state_files-permission_permissive`.
- **Detector:** Not measured. The inventory found no check that reads file modes.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.

## fm-state_files-workspace-ambiguous-multiple-candidates

- **Label:** UNCLASSIFIED
- **Severity:** P1. Not in the scored population: workspace resolution happens before doctor runs, outside the surface the population was built from.
- **Detector:** Not measured. The fixture is marker-only.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.
