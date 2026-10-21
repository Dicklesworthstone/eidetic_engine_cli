# fm-state_files-sqlite-integrity-page-malformed

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-state_files-sqlite-integrity-page-malformed` |
| Severity | P0 |
| Subsystem | state_files |
| Repair spec | [`docs/doctor/repair-specs/state_files.md#fm-state_files-sqlite-integrity-page-malformed`](../../../docs/doctor/repair-specs/state_files.md#fm-state_files-sqlite-integrity-page-malformed) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-state_files-sqlite-integrity-page-malformed.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). It checks the
   independent witness named in the spec's Oracle, confirms the pre-fix report
   carries `posture` `blocked` with both `database`/EE-E207 and
   `search_index`/EE-E300 in `actionable`, then runs
   `doctor_fixture_assert_guidance_only` in `lib.sh`: `ee doctor --fix` exits 6
   with one `database_unavailable` fixer whose `operation` is `manual` and
   `outcome` is `guidance_recorded`, the content digest is byte-identical
   across the fix, and `.ee/index-rebuild-request.json` stays ABSENT. No undo
   step runs because nothing is written.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **GUIDANCE-ONLY** (repair spec and `manifest.json`). This fixture was a
pinned gap: doctor reported the malformed page as healthy, and a passing run
meant the gap was still there. A detector has since landed, so `ee doctor` now
reports EE-E207 with posture `blocked`, and `ee doctor --fix` records manual
guidance instead of rewriting anything. The relabelling reached
`manifest.json` and `assert.sh` but not this README or the repair spec, which
is what the fixture contract caught.
There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with `EE_DOCTOR_FIXTURE_RUN_EE=1`
when an `ee` binary is on `PATH`; its caller, the `ee doctor Safety Harness`
stage of `scripts/verify.sh`, is not run by any CI workflow (bd-feftl).
