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
only when the CLI wiring for `ee doctor --fix --only <fm>` is available.
