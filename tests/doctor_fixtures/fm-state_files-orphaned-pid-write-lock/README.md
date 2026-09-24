# fm-state_files-orphaned-pid-write-lock

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-state_files-orphaned-pid-write-lock` |
| Severity | P1 |
| Subsystem | state_files |
| Repair spec | [`docs/doctor/repair-specs/state_files.md#fm-state_files-orphaned-pid-write-lock`](../../../docs/doctor/repair-specs/state_files.md#fm-state_files-orphaned-pid-write-lock) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-state_files-orphaned-pid-write-lock.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` requires `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary in
   `EE_DOCTOR_FIXTURE_BINARY` (it exits 2 without them). It starts the
   holder that `corrupt.sh` wrote (`.fixture_baseline/hold-write-lock.py`: an
   exclusive `flock(2)` on `.ee/ee.write.lock`, then no progress). It stops the
   holder before returning, so no process outlives the fixture. Its
   independent witness is a second non-blocking flock being refused. It pins
   defect bd-ixxzq as it is today: `ee doctor` reports posture `blocked` with
   `database` `EE-E202` ("write lock holder made no progress"), and an unscoped
   `ee doctor --fix` exits 6, recording `database_corrupted` manual guidance
   for a store that is not damaged. With the holder stopped, doctor is healthy
   and the content digest is unchanged. One held doctor run takes about 150 s.
   The fixture needs `python3` on `PATH`.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **PINNED-DEFECT bd-ixxzq** (NOT coverage; repair spec and
`manifest.json`). The orphan as named self-heals: the kernel releases a flock
when its process dies. So the fixture is re-scoped to a LIVE holder that makes
no progress (bd-2oh15 c9984/c9986). Doctor reports that held lock as database
corruption. A passing run means the defect is still there; when bd-ixxzq gives
a held lock its own finding, the fixture goes red on purpose and is relabelled.
There is no per-FM fix: `ee doctor --fix --only <id>` is a usage error,
because `--fix` declares a conflict with `--only`.
`scripts/verify-undo.sh` runs this fixture with `EE_DOCTOR_FIXTURE_RUN_EE=1`
when an `ee` binary is on `PATH`; its caller, the `ee doctor Safety Harness`
stage of `scripts/verify.sh`, is not run by any CI workflow (bd-feftl).
`scripts/verify-idempotence.sh` and `scripts/verify-metamorphic.sh` run every
doctor call through `condition.sh`, which starts the holder, confirms the lock
is held, and stops it afterwards. If the holder cannot take the lock, the run
counts as `condition_not_applied`: never a pass, and the harness fails. Each of
those harnesses adds about two held doctor runs (about 150 s each).
