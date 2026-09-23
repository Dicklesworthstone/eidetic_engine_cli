# state_files repair specs

Repair specs for the `state_files` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-state_files-sqlite-wal-shm-sidecar-drift

- **Label:** UNRESOLVED
- **Severity:** P0. Scored P0 as `pop-wal_shm-stale_sidecar` in `docs/doctor/failure_mode_scores.jsonl`.
- **Detector:** None known. No doctor check reads `.ee/ee.db-wal` contents or `.ee/ee.db-shm`; `wal_pressure` (EE-E205) compares WAL size only.
- **Real trigger:** Not yet built. Random sidecar bytes produce no failure: fsqlite-wal rejects frames whose salts or checksum chain disagree with the WAL's OWN header (fsqlite-wal-0.4.0 `checksum.rs`). It does not compare the WAL with the database header, and ee reads only the WAL's size (`wal_status`, `wal_pressure`) and never the `-shm`. So the recipe to measure is an internally consistent WAL captured with an older `ee.db`, placed beside a newer `ee.db`: it would pass validation and replay. This is a code reading from the bd-2oh15 survey, not a measurement; the fsqlite-pager open path was not traced, so a database-binding check there is not ruled out.
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

- **Label:** NOT-DETECTED (pinned gap; NOT coverage; bd-2oh15 ruling c9986)
- **Severity:** P1. Scored P1 as `pop-beads_jsonl-tombstone_drift`, a row added after the blind inventory (bd-2oh15 c9985/c9986), because doctor has no JSONL export check.
- **Detector:** None. `src/core/doctor_fixers.rs` declares the family as FM-SF-02 (`fix_beads_jsonl_drift`, finding `beads_jsonl_drift`: rewrite the export from the database), but no doctor check produces that finding, so it is never dispatched.
- **Real trigger:** A real `br` tracker in the workspace (`br init`, two issues, `br sync --flush-only`). One issue is deleted with `br delete` and flushed, so the database and the export both hold its tombstone. The export is then MOVED into `.fixture_baseline/` and replaced by the export taken before the delete: it has lost the tombstone and carries the deleted issue as live, which an import would resurrect. `br doctor --json` reports the tracker degraded (`ok: false`). The fixture needs `br` and `sqlite3` on `PATH`.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable. The export is unchanged by `--fix`.
- **Oracle:** `doctor_fixture_assert_pinned_gap` on `.beads/issues.jsonl`: doctor healthy with an empty `actionable`, `--fix` reports 0 actions, and the export bytes are unchanged. The witness is read-only: `sqlite3 -readonly` reads the deleted issue's status as `tombstone` in `.beads/beads.db` while the export carries the same id with another status.
- **Negative control:** Before the export is replaced, it carries the tombstone, the database agrees, and `br doctor` reports `ok: true` (`corrupt.sh`). A measured control that restores the tombstone-bearing export makes `assert.sh` fail with "witness lost".
- **Pinned sha:** 25a9d7f8d, measured on its stamped release build on vmi1227854 with `br` 0.6.0 (corrupt 0, assert 0: "pinned gap confirmed"; the restored-tombstone control failed with "witness lost"). `br doctor` reported `ok: false` with `base_jsonl`, `db.export_hash_cache` and `sync.metadata` on the drifted tracker.

## fm-state_files-orphaned-pid-write-lock

- **Label:** PINNED-DEFECT bd-ixxzq (NOT coverage; re-scoped to the live holder by the bd-2oh15 rulings c9984/c9986)
- **Severity:** P1. Scored P1 as `pop-ee_db-locked`.
- **Detector:** Misattributed. An orphan as named self-heals: the kernel releases a flock when its process dies, so there is no orphaned state to detect. What remains is a LIVE writer that holds `.ee/ee.write.lock` and makes no progress. The `database` check waits out the flock gate (38 s without progress, `src/db/mod.rs`) and reports `EE-E202` with "database write lock holder made no progress", and `search_index` reports `EE-E300`, posture `blocked`. `fix_finding_for_check` maps every `database` `EE-E202` to `database_corrupted` (bd-ixxzq), so a held lock inherits corruption guidance.
- **Real trigger:** A healthy store, and a holder (`.fixture_baseline/hold-write-lock.py`) that takes an exclusive `flock(2)` on the existing write lock (opened for reading, so no byte changes) and then does nothing. `assert.sh` runs the holder only while it measures and stops it before it returns, so no process outlives the fixture. The fixture needs `python3` on `PATH`. One held `ee doctor` run takes about 150 s.
- **Repair:** Wrong guidance, pinned. `--fix` exits 6 `completed_partial` with `database_corrupted` / `manual` / `guidance_recorded`: copy `ee.db` aside, restore a backup, or "move `.ee/ee.db` aside and run `ee init`", for a store that is not damaged. The target is a finding of its own for a held lock (wait for or stop the holder), after which this fixture goes red on purpose and is relabelled.
- **Undo:** Not applicable. Guidance writes nothing outside doctor's run records.
- **Oracle:** `assert.sh` pins the defect: the witness (a second non-blocking exclusive flock is refused while the holder lives); doctor `blocked` with `database` `EE-E202` and the lock message; `--fix` exit 6 with only `guidance_recorded` results, including `database_corrupted`; then, with the holder stopped, doctor healthy and the content digest unchanged. That last step shows the store was never damaged.
- **Negative control:** With no holder the lock probe reads `free` and doctor is healthy (`corrupt.sh`).
- **Pinned sha:** 25a9d7f8d, measured on its stamped release build on vmi1227854 (corrupt 0, assert 0: "pinned defect confirmed" in 311 s; no holder process left afterwards). A control whose holder takes no lock failed with "witness lost". The same codes were first seen on 02524267e (held doctor 152 s, held `--fix` 153 s, released doctor healthy in 0 s).

## fm-state_files-permissions-too-permissive

- **Label:** NOT-DETECTED (pinned gap; NOT coverage)
- **Severity:** P1. Scored P1 as `pop-state_files-permission_permissive`.
- **Detector:** None. No doctor check reads file modes, and the chmod fixer `fix_state_file_permission_drift` is never dispatched by `--fix`.
- **Real trigger:** A healthy store whose private modes (`ee init` sets `.ee` 0700 and `ee.db` 0600) are opened to `.ee` 0755 and `ee.db` 0644, so any local user can read the memory store. `corrupt.sh` fails unless the baseline was private and the store is group/other readable afterwards.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable. The modes are still open after `--fix`.
- **Oracle:** `doctor_fixture_assert_pinned_gap` on `.ee/ee.db`: doctor healthy with an empty `actionable`, `--fix` reports 0 actions, and the database bytes are unchanged. The witness is the mode string from `ls -ld` (portable across GNU and BSD): `ee.db` is group- and other-readable, and `.ee` is group- and other-readable and enterable, before and after `--fix`. (A first version used `find -perm`, whose flag text trips the contract's forbidden-token scan; bd-2oh15 c10006.)
- **Negative control:** `corrupt.sh` requires the healthy store's baseline modes to be private before opening them up; `doctor_fixture_healthy_store` asserts the store is healthy first.
- **Pinned sha:** 02524267e, measured on its stamped release build on hz3 (corrupt 0, assert 0: "pinned gap confirmed").

## fm-state_files-workspace-ambiguous-multiple-candidates

- **Label:** NOT-DETECTED (pinned gap; NOT coverage; bd-2oh15 ruling c9986)
- **Severity:** P1. Scored P1 as `pop-workspace_selection-explicit_environment_conflict`, a row added after the blind inventory (bd-2oh15 c9985/c9986), because workspace resolution happens before doctor runs.
- **Detector:** None. `diagnose_workspace_resolution` (`src/config/workspace.rs`) reports `workspace_explicit_environment_conflict` when `--workspace` and `EE_WORKSPACE` name different initialized workspaces, but only `ee workspace resolve` and `ee status` surface it. Doctor runs against the explicit workspace and reports it healthy. This matches `fm-workspace_config-nested-ee-markers`.
- **Real trigger:** Two healthy initialized workspaces are both candidates for one command: `--workspace` names the target and `EE_WORKSPACE` names a second workspace under `.fixture_baseline/other-workspace` (outside the content digest). The environment is carried in `.fixture_baseline/env.sh`. Resolution runs from inside the target, so the workspace discovered from the current directory is the target itself and adds no second finding.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable. Nothing is written.
- **Oracle:** `doctor_fixture_assert_pinned_gap` (artifact `-`, since the damage is the environment) run under the ambiguous environment from inside the target: doctor healthy with an empty `actionable`, and `--fix` reports 0 actions. The witness is `ee workspace resolve` reporting exactly `["workspace_explicit_environment_conflict"]`.
- **Negative control:** Without `EE_WORKSPACE` the same resolve reports no diagnostics (`corrupt.sh`). A measured control that points `EE_WORKSPACE` at the target itself makes `assert.sh` fail with "witness lost".
- **Pinned sha:** 25a9d7f8d, measured on its stamped release build on vmi1227854 (corrupt 0, assert 0: "pinned gap confirmed"; the agreeing-environment control failed with "witness lost").
