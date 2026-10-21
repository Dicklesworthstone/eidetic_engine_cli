# fm-workspace_config-config-toml-malformed

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-workspace_config-config-toml-malformed` |
| Severity | P1 |
| Subsystem | workspace_config |
| Repair spec | [`docs/doctor/repair-specs/workspace_config.md#fm-workspace_config-config-toml-malformed`](../../../docs/doctor/repair-specs/workspace_config.md#fm-workspace_config-config-toml-malformed) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-workspace_config-config-toml-malformed.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). Its independent
   witness is `ee search`, which must still fail with a `configuration` error
   on the malformed `.ee/config.toml`. It then runs
   `doctor_fixture_assert_pinned_gap` in `lib.sh`: `ee doctor` must report the
   workspace healthy, an unscoped `ee doctor --fix` must take 0 actions, and
   the config must be byte-identical afterwards. No undo step runs because
   nothing is written.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **REPAIR** (repair spec and `manifest.json`). This fixture was a pinned
gap: doctor swallowed `.ee/config.toml` parse errors, reported the workspace
healthy, and a passing run meant the gap was still there. A detector landed
(EE-E101, posture `blocked`) and was joined to
`fix_workspace_config_malformed_toml`, so `ee doctor --fix` now quarantines the
malformed config by rename and `ee doctor undo` puts the damage back. The
relabelling reached `manifest.json` and `assert.sh` but not this README, which
is what the fixture contract caught.
There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with `EE_DOCTOR_FIXTURE_RUN_EE=1`
when an `ee` binary is on `PATH`; its caller, the `ee doctor Safety Harness`
stage of `scripts/verify.sh`, is not run by any CI workflow (bd-feftl).
