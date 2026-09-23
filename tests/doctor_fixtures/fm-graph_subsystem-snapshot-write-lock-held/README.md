# fm-graph_subsystem-snapshot-write-lock-held

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-graph_subsystem-snapshot-write-lock-held` |
| Severity | P1 |
| Subsystem | graph_subsystem |
| Repair spec | [`docs/doctor/repair-specs/graph_subsystem.md#fm-graph_subsystem-snapshot-write-lock-held`](../../../docs/doctor/repair-specs/graph_subsystem.md#fm-graph_subsystem-snapshot-write-lock-held) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-graph_subsystem-snapshot-write-lock-held.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). It checks the
   independent witness named in the spec's Oracle, then runs
   `doctor_fixture_assert_pinned_gap` in `lib.sh`: `ee doctor` must report the
   workspace healthy and an unscoped `ee doctor --fix` must take 0 actions. No
   undo step runs because nothing is written.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **NOT-DETECTED** (pinned gap; NOT coverage; repair spec and
`manifest.json`). Doctor reports this damage as healthy, so `ee doctor --fix`
dispatches nothing for it. A passing run means the gap is still there; when a
detector lands the fixture goes red on purpose and is relabelled.
There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with `EE_DOCTOR_FIXTURE_RUN_EE=1`
when an `ee` binary is on `PATH`; its caller, the `ee doctor Safety Harness`
stage of `scripts/verify.sh`, is not run by any CI workflow (bd-feftl).
