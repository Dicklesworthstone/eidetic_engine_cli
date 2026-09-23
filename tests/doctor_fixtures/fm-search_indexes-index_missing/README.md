# fm-search_indexes-index_missing

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-search_indexes-index_missing` |
| Severity | P1 |
| Subsystem | search_indexes |
| Repair spec | [`doctor_workspace/analysis/repair_specs/search_indexes.md`](../../../doctor_workspace/analysis/repair_specs/search_indexes.md) |

## Guidance-only contract (bd-2oh15, orchestrator decision C)

This is a guidance-only failure mode. The real repair, `ee index rebuild
--workspace <target> --json`, writes SQLite rows, the write-lock counter, WAL
sidecars and a timestamped `meta.json`. None of that can pass through
`doctor_runtime::mutate()` or be reversed by the file-only `doctor --undo`, so
`doctor --fix` keeps `RunIndexRebuild` advisory and records guidance. The
fixture asserts that doctor says so honestly. It does not assert a repair
round trip.

1. Provide an empty target directory and a real, prebuilt `ee` through
   `EE_DOCTOR_FIXTURE_BINARY`. `corrupt.sh` refuses nonempty or symlink targets.
2. It runs `ee init --skip-boilerplate --json`, remembers a real source memory,
   and rebuilds the index, requiring at least one memory indexed. It requires
   the shared health assertion to pass before altering anything. The database
   and populated search index must exist; an empty corpus or uninitialized
   directory is not a substitute.
3. It moves `.ee/index` into `.fixture_baseline/healthy-index`, preserving every
   byte. No file is deleted.
4. Real doctor output must identify `search_index` warning `EE-E300`, with
   degraded core health and every other core check still `ok`. Both the healthy
   and corrupted reports are retained under `.fixture_baseline/`.
5. With `EE_DOCTOR_FIXTURE_RUN_EE=1`, `assert.sh` runs
   `doctor_fixture_assert_guidance_only`: `doctor --fix` must exit 0 and report
   `search_index_missing` as `guidance_recorded` (never `applied`, with
   `guidanceOnlyFixerCount >= 1`); the next `doctor` must still report
   `search_index` `EE-E300`; and `.ee/index` must still be absent. Without the
   flag it refuses; marker-only success is forbidden.

The guidance-only contract does not compare a byte digest: doctor's own run
bookkeeping (lock and WAL sidecars) is not excluded from the digest, and a
guidance-only fix has nothing to undo.

## Wiring status

`scripts/verify-undo.sh` runs this fixture through the existing safety harness.
The helper's controls, including a false-green for each way the fix could
misreport, are in `tests/doctor_fixtures/assertion_contract.py`
(`GuidanceOnlyContract`).

If doctor ever performs this rebuild through the chokepoint with an undo whose
oracle is source-of-truth equivalence (decision A, once bd-cjt23's recordsHash
exists), this fixture will go red on "no longer reports". That is correct. It
should then be converted back to a repair round trip, not relaxed. Do not
substitute the explicit rebuild into `assert.sh`. The cited independent repair
spec remains absent (bd-2oh15).
