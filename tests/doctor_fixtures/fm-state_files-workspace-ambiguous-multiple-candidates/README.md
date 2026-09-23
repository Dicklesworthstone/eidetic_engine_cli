# fm-state_files-workspace-ambiguous-multiple-candidates

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-state_files-workspace-ambiguous-multiple-candidates` |
| Severity | P1 |
| Subsystem | state_files |
| Repair spec | [`docs/doctor/repair-specs/state_files.md#fm-state_files-workspace-ambiguous-multiple-candidates`](../../../docs/doctor/repair-specs/state_files.md#fm-state_files-workspace-ambiguous-multiple-candidates) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-state_files-workspace-ambiguous-multiple-candidates.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). It sources
   `.fixture_baseline/env.sh`, which sets `EE_WORKSPACE` to a second
   initialized workspace under `.fixture_baseline/`, while `--workspace`
   names the target. Its independent witness is `ee workspace resolve`, run
   from inside the target, reporting exactly
   `workspace_explicit_environment_conflict`. It then runs
   `doctor_fixture_assert_pinned_gap` in `lib.sh` under the same environment:
   `ee doctor` must report the workspace healthy and an unscoped
   `ee doctor --fix` must take 0 actions. No undo step runs because nothing
   is written.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **NOT-DETECTED** (pinned gap; NOT coverage; repair spec and
`manifest.json`). Doctor reports this workspace as healthy, so
`ee doctor --fix` dispatches nothing for it. A passing run means the gap is
still there; when a detector lands the fixture goes red on purpose and is
relabelled.
There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with `EE_DOCTOR_FIXTURE_RUN_EE=1`
when an `ee` binary is on `PATH`; its caller, the `ee doctor Safety Harness`
stage of `scripts/verify.sh`, is not run by any CI workflow (bd-feftl). The
other counting harnesses run doctor on this target without
`.fixture_baseline/env.sh`, so there it only checks a healthy workspace.
