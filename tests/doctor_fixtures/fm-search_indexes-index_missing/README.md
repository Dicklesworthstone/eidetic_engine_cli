# fm-search_indexes-index_missing

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-search_indexes-index_missing` |
| Severity | P1 |
| Subsystem | search_indexes |
| Repair spec | [`doctor_workspace/analysis/repair_specs/search_indexes.md`](../../../doctor_workspace/analysis/repair_specs/search_indexes.md) |

## Round-trip contract

1. Provide an empty target directory and a real, prebuilt `ee` through
   `EE_DOCTOR_FIXTURE_BINARY`. `corrupt.sh` refuses nonempty or symlink targets.
2. It runs `ee init --skip-boilerplate --json`, remembers a real source memory,
   and rebuilds the index, requiring at least one memory indexed. It requires
   the shared health assertion to pass before altering anything. The database
   and populated search index must exist; an empty corpus or uninitialized
   directory is not a substitute.
3. It moves `.ee/index` into `.fixture_baseline/healthy-index`, preserving every
   byte, then captures the corrupted pre-fix content digest. No file is deleted.
4. Real doctor output must identify `search_index` warning `EE-E300`, with
   degraded core health and every other core check still `ok`. Both the healthy
   and corrupted reports are retained under `.fixture_baseline/`.
5. With `EE_DOCTOR_FIXTURE_RUN_EE=1`, `assert.sh` runs `doctor --fix`, the
   read-only health assertion, then `doctor --undo <runId>` and the content
   comparison. Without the flag it refuses; marker-only success is forbidden.

## Wiring status

`scripts/verify-undo.sh` runs this fixture through the existing safety harness.
The CLI forbids combining `--fix` with `--only`; they are separate calls in the
shared helper, and `--only` is currently advisory.

The index rebuild repair is `ee index rebuild --workspace <target> --json`.
Currently `doctor --fix` selects `RunIndexRebuild`, whose runtime implementation
only records manual guidance. Thus a successfully induced missing index is
expected to keep the full doctor round trip **red** until that production path
actually rebuilds it and supports undo. Do not substitute the explicit rebuild
into `assert.sh` or change its baseline to make that round trip pass. An explicit
rebuild can independently establish restored health, but cannot prove doctor
repair or undo. The cited independent repair spec remains absent (bd-2oh15).
