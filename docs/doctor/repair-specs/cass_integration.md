# cass_integration repair specs

Repair specs for the `cass_integration` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-cass_integration-cass_not_found

- **Label:** GUIDANCE-ONLY (report-only)
- **Severity:** P0. Scored P1 as `pop-cass-absent` in `docs/doctor/failure_mode_scores.jsonl`: cass is an optional integration, and its absence disables session import only.
- **Detector:** `cass` reports EE-E506, in `doctor --full` only. Concise doctor shows nothing.
- **Real trigger:** The failure is environmental. `corrupt.sh` records, in `.fixture_baseline/path-without-cass`, the current `PATH` with every directory holding a `cass` executable removed; doctor runs under that `PATH`.
- **Repair:** None. EE-E506 has no dispatch in `doctor_fix_json`, so `--fix` reports 0 actions.
- **Undo:** Not applicable: nothing is written.
- **Oracle:** `doctor_fixture_assert_report_only` for `cass` EE-E506 in full mode: detected before and after `--fix`, 0 actions.
- **Negative control:** `corrupt.sh` refuses to run if `jq` or `shasum` would also disappear from the reduced `PATH`, so the finding cannot come from a broken environment.
- **Pinned sha:** 9188a0661, measured on the ab83e3f23 release binary.

## fm-cass_integration-cass_unavailable-contract-mismatch

- **Label:** UNCLASSIFIED
- **Severity:** P1. Scored P1 as `pop-cass-contract_mismatch`.
- **Detector:** Not measured by this fixture. The inventory shows `cass` reports EE-E507, which `doctor --fix` dispatches to `fix_cass_integration_drift`.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.
