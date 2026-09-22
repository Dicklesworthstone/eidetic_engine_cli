# Doctor Fixture Suite

This directory holds per-failure-mode fixtures for the `ee doctor --fix`
surface. Each fixture has:

- `README.md` with the failure-mode id, severity, subsystem, and repair-spec source.
- `corrupt.sh` to build an isolated corrupt workspace state.
- `assert.sh` to validate the fixture state and, when explicitly enabled, run the public doctor flow.

The scripts are intentionally conservative. They retain all temporary data for
audit, avoid deletion, avoid Git mutation, and never run Cargo. The Rust
contract test `tests/doctor_fixtures_contract.rs` validates that every P0/P1
failure mode in `doctor_workspace/failure_mode_scores.jsonl` has this triplet.

To run the shell fixtures manually:

```bash
EE_DOCTOR_FIXTURE_TARGET=/tmp/ee-doctor-fixture \
  tests/doctor_fixtures/fm-state_files-sqlite-wal-shm-sidecar-drift/corrupt.sh

EE_DOCTOR_FIXTURE_TARGET=/tmp/ee-doctor-fixture \
  tests/doctor_fixtures/fm-state_files-sqlite-wal-shm-sidecar-drift/assert.sh
```

Set `EE_DOCTOR_FIXTURE_RUN_EE=1` and `EE_DOCTOR_FIXTURE_BINARY=/path/to/ee`
to exercise fix, the read-only health report, and undo. Without that flag,
`assert.sh` checks only marker existence; it does not verify a repair.
`scripts/verify-undo.sh` sets the flag when the safety harness runs.

The shared helper requires one successful doctor response with `posture: ok`,
`healthy: true`, a nonempty `coreChecks` array containing only `ok` checks, and
no actionable checks. A successful process exit alone is insufficient.
Undo compares a digest of regular-file paths **and their contents** against
the baseline captured by `corrupt.sh`; it retains the existing audit/capture
exclusions in `lib.sh`. It does not check permissions, symlinks, or empty
directories. Baselines made by the old filename-only helper must be recaptured.

Run the assertion-helper regression controls with:

```bash
python3 tests/doctor_fixtures/assertion_contract.py
```

`scripts/run-safety-harness.sh` also executes these controls before running the
per-FM harnesses. They use a doctor test double to plant unhealthy/missing
reports, unsuccessful process exits, and incomplete byte restoration, alongside
valid round trips. They retain their workspaces and receipts under temporary
scratch, or under `EE_DOCTOR_ASSERTION_TEST_ROOT` when provided. Passing them
verifies the assertion helper, not real doctor repairs.

The suite remains incomplete (bd-2oh15): the 25 corruption scripts currently
write markers rather than their named corruptions; the independently scored
failure-mode population and eight referenced repair specs are absent. Also,
`doctor --only` is currently advisory, so the after-report does not establish
per-FM detector coverage. The helper's health check follows the doctor's core
health contract; it does not certify optional advisory subsystems as repaired.
