# search_indexes repair specs

Repair specs for the `search_indexes` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-search_indexes-index_missing

- **Label:** REPAIR
- **Severity:** P1. Scored P1 as `pop-index-missing` in `docs/doctor/failure_mode_scores.jsonl`.
- **Detector:** `search_index` reports EE-E300.
- **Real trigger:** A real indexed store whose `.ee/index` directory is absent.
- **Repair:** `fix_search_index_missing` dispatches the `run_index_rebuild` operation; `fixerResults` records finding `search_index_missing` with outcome `applied`.
- **Undo:** `doctor --undo <runId>` removes the rebuilt index and restores the exact source baseline; a second undo reports 0 actions.
- **Oracle:** `--fix` status `completed_ok` with no guidance-only fixers; after undo, doctor reports EE-E300 again, and the content digest after the second undo equals the pre-fix baseline. The write lock is monotonic.
- **Negative control:** After `--fix`, doctor reports healthy; after undo, EE-E300 returns.
- **Pinned sha:** 476df83df.

## fm-search_indexes-index_stale

- **Label:** REPAIR
- **Severity:** P1. Scored P1 as `pop-index-stale`.
- **Detector:** `search_index` reports EE-E301: the store's `workspace_generations.generation` is ahead of the generation recorded in `.ee/index/meta.json`.
- **Real trigger:** A real indexed store whose workspace generation is advanced by one past the index's (what writes after the last rebuild leave behind). `corrupt.sh` requires the two generations equal before the edit and the store ahead after it. Doctor's lock is provisioned by a healthy no-op `--fix` first.
- **Repair:** `fix_search_index_stale` dispatches the `run_index_rebuild` operation; `fixerResults` records finding `search_index_stale` with outcome `applied`, and doctor is healthy afterwards.
- **Undo:** `doctor --undo <runId>` restores the exact stale baseline (the index's generation is the old one again and EE-E301 returns); a second undo reports 0 actions.
- **Oracle:** `doctor_fixture_assert` round trip (fix, health, undo, content digest equal to the corrupted baseline, monotonic write lock), plus `completed_ok` with no guidance-only fixers, the applied `run_index_rebuild`, EE-E301 after undo, and an idempotent second undo.
- **Negative control:** `doctor_fixture_healthy_store` asserts the store is healthy with equal generations before the edit.
- **Pinned sha:** 02524267e, measured on its stamped release build on hz3.

## fm-search_indexes-index_corrupt

- **Label:** REPAIR
- **Severity:** P1. Scored P1 as `pop-index-corrupt`: the index is a derived artifact that a rebuild recovers from the intact store. The manifest said P0 until the bd-2oh15 c9891 ruling (the rubric governs).
- **Detector:** `search_index` reports EE-E301. Corrupt tier files are reported as stale.
- **Real trigger:** The largest non-meta file under `.ee/index` is replaced with random bytes of the same length. Its relative path is recorded in `.fixture_baseline/corrupted-index-file`.
- **Repair:** `fix_search_index_stale` dispatches the `run_index_rebuild` operation; `fixerResults` records finding `search_index_stale` with outcome `applied`.
- **Undo:** `doctor --undo <runId>` restores the exact corrupted baseline, so EE-E301 returns; a second undo is a no-op.
- **Oracle:** `--fix` status `completed_ok` with no guidance-only fixers; doctor is healthy after the fix; after undo the corrupted file is back and doctor reports EE-E301.
- **Negative control:** `doctor_fixture_healthy_store` asserts the undamaged store is healthy before the damage.
- **Pinned sha:** 9188a0661, measured on the ab83e3f23 release binary.
