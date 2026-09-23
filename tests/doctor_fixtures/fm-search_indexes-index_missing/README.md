# fm-search_indexes-index_missing

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-search_indexes-index_missing` |
| Severity | P1 |
| Subsystem | search_indexes |
| Repair spec | [`docs/doctor/repair-specs/search_indexes.md#fm-search_indexes-index_missing`](../../../docs/doctor/repair-specs/search_indexes.md#fm-search_indexes-index_missing) |

## Repair round-trip contract (bd-2oh15, option A)

`doctor --fix` rebuilds a missing index from a read-only source snapshot and
journals every live-index change through `doctor_runtime::mutate()`, so
`doctor --undo` reverses it (7b2af4685, cd79e4ea7, 1f3cda72f, 33e5008f3).

1. Provide an empty target directory and a real, prebuilt `ee` through
   `EE_DOCTOR_FIXTURE_BINARY`. `corrupt.sh` refuses nonempty or symlink targets.
2. It runs `ee init --skip-boilerplate --json`, remembers a real source memory
   that carries explicit path and symbol anchors, and rebuilds the index,
   requiring at least one memory indexed and a healthy baseline. The anchors
   make `memory_anchors` / `memory_anchor_index` non-empty, so a repair that
   rewrote them would change source bytes.
3. A healthy no-op `doctor --fix` provisions the persistent `.ee/.doctor.lock`
   before the baseline, so that lock is byte-compared like any other file.
4. It moves `.ee/index` into `.fixture_baseline/healthy-index`, preserving every
   byte, then records the content digest and the `ee.write.lock` epoch. No file
   is deleted. Real doctor output must identify `search_index` `EE-E300`.
5. With `EE_DOCTOR_FIXTURE_RUN_EE=1`, `assert.sh` asserts: fix `applied` for
   `search_index_missing` (never guidance); post-fix core health ok; undo
   `undone` with the index absent again and `EE-E300` reported again; a second
   undo is a no-op; and the content digest equals the baseline after both
   undos. Without the flag it refuses; marker-only success is forbidden.

How the digest treats runtime files (orchestrator decision on bd-2oh15 c9818,
"classify, do not ignore"). A real fix + undo changed exactly two files outside
`.doctor/`:
- `ee.db-shm` is excluded: a transient SQLite shared-memory index derived from
  the WAL.
- `ee.write.lock` is excluded from the bytes but checked by its semantics. It
  must still exist after undo, and its epoch (20 digits + LF) must be >= the
  baseline. It is a monotonic counter no honest undo can restore.
Everything else, including `ee.db`, `ee.db-wal` and `.doctor.lock`, is
byte-compared.

Verified on real binaries: an A binary built at 33e5008f3 passes (fix
applied, undo undone with 15 actions, write-lock epoch 84 -> 100). The C-era
binary built at 369c63544 fails at "post-fix health not established".

## Wiring status

Label: **REPAIR** (repair spec and `manifest.json`). Doctor reports the
missing index as `EE-E300` and `ee doctor --fix` rebuilds it.
`scripts/verify-undo.sh` runs this fixture through the existing safety harness.
`tests/doctor_fixtures/assertion_contract.py` pins the digest classification:
shm noise is ignored, a WAL change is rejected, and a write lock that goes
missing, becomes unreadable, goes backwards, or appears when it was absent is
rejected. The cited independent repair spec remains absent (bd-2oh15).
