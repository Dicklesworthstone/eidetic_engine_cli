# fm-policy_safety-trauma-guard-policy-denied-exit-7

| Field | Value |
| --- | --- |
| Failure-mode id | `fm-policy_safety-trauma-guard-policy-denied-exit-7` |
| Severity | P1 |
| Subsystem | policy_safety |
| Repair spec | [`docs/doctor/repair-specs/policy_safety.md#fm-policy_safety-trauma-guard-policy-denied-exit-7`](../../../docs/doctor/repair-specs/policy_safety.md#fm-policy_safety-trauma-guard-policy-denied-exit-7) |

## Round-trip contract

Per `bd-2oh15`, the fixture lifecycle is:

1. `corrupt.sh` builds an isolated corrupt workspace at
   `$EE_DOCTOR_FIXTURE_TARGET` and writes the marker
   `.ee/doctor-fixtures/fm-policy_safety-trauma-guard-policy-denied-exit-7.json`, plus a baseline
   `.fixture_baseline/before.sha256`.
2. `assert.sh` confirms the marker is present. When
   `EE_DOCTOR_FIXTURE_RUN_EE=1` and a binary is provided in
   `EE_DOCTOR_FIXTURE_BINARY`, `doctor_fixture_assert` in `lib.sh`
   additionally runs an unscoped `ee doctor --fix`, a follow-up `ee doctor`
   report (the `--only` it passes filters nothing without `--fix`), then
   `ee doctor --undo <runId>`, and finally compares the post-undo SHA-256
   manifest against the pre-fix baseline (round-trip byte-identical). The
   marker is not real damage, so this round trip exercises undo only.

The shell scripts intentionally NEVER invoke Cargo and NEVER
delete files. Recovery, including the post-undo step, runs
through the read-only `corrupt` -> `marker write` -> `doctor`
-> `undo` sequence so an operator can audit every intermediate
state on disk.

## Wiring status

Label: **OUT-OF-SCOPE** (repair spec and `manifest.json`, whose
`scopeReason` carries the category, citations and search). A trauma-guard
policy denial exiting 7 is a correct command outcome, not damaged workspace
state, so this is not a doctor failure mode (bd-2oh15 ruling on c9985). The
scripts above are still marker-only, and no harness runs them: every counting
sub-harness skips this id, lists it on its OUT-OF-SCOPE line, and never counts
it as a pass. The id is pinned exactly in `DOCTOR_FIXTURE_OUT_OF_SCOPE_IDS` in
`lib.sh`.
