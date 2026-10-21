# workspace_config repair specs

Repair specs for the `workspace_config` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-workspace_config-nested-ee-markers

- **Label:** NOT-DETECTED (pinned gap; NOT coverage; bd-2oh15 rulings c9985/c9986)
- **Severity:** P1. Scored P1 as `pop-workspace_tree-nested_ee_markers`, a row added after the blind inventory (bd-2oh15 c9985/c9986), because nested `.ee` markers affect workspace resolution before doctor runs.
- **Detector:** None. `diagnose_workspace_resolution` (`src/config/workspace.rs`) reports `workspace_nested_markers` when the current directory's ancestry holds more than one initialized workspace, but only `ee workspace resolve` and `ee status` surface it; doctor never calls it, and no doctor check looks for a store nested inside the workspace.
- **Real trigger:** A healthy workspace, plus a second one initialized inside it at `nested/child`. From the child directory the nearest marker wins unless `--workspace` is explicit, so a command meant for one store can write to the other. The nested store is part of the target's own bytes.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable. Nothing is written.
- **Oracle:** `doctor_fixture_assert_pinned_gap` (artifact `-`), run from inside the child on the outer workspace: doctor healthy with an empty `actionable`, `--fix` reports 0 actions. The witness is `ee workspace resolve` from the child reporting exactly `["workspace_nested_markers"]`.
- **Negative control:** Before the child is initialized, the same resolve from the same directory reports no diagnostics (`corrupt.sh`).
- **Pinned sha:** fa6967499, measured on its stamped release build on an RCH worker (corrupt 0, assert 0: "pinned gap confirmed"; with the child's `.ee` moved aside the witness fails with "witness lost").

## fm-workspace_config-config-toml-malformed

- **Label:** REPAIR (full round trip). Upgraded from NOT-DETECTED under spec rule V6 on 2026-10-14 (bd-ooj2h), because the pinned-gap assertion started failing with "GAP no longer reproduces; the label is wrong, relabel it" — which is that assertion doing exactly what it was written to do on the day doctor learns to detect something. The bucket was spelled COVERAGE here while `manifest.json` already said REPAIR; the fixture contract requires this field to START with the manifest label, so the two had drifted in vocabulary rather than in meaning.
- **Severity:** P1. Scored P1 as `pop-config-malformed` in `docs/doctor/failure_mode_scores.jsonl`. Note that row lists BOTH this fixture and `fm-state_files-merge-conflict-markers`, i.e. two fixtures for one failure mode.
- **Detector:** `workspace`, error code `EE-E101`, posture `blocked`. The previous entry said "None. Doctor swallows `config.toml` parse errors"; that is no longer true. Repair hint as emitted: "Fix the TOML syntax in the config file the error names, then re-run the command".
- **Real trigger:** A valid comment-only `.ee/config.toml` that `ee search` accepts is moved into `.fixture_baseline/` and replaced by the same content plus invalid TOML (an unclosed `[search` table header and a key without a value). `ee search` then fails with an `ee.error.v2` envelope whose code is `configuration`.
- **Repair:** `fix_workspace_config_malformed_toml` (src/core/doctor_fixers.rs:309), dispatched for EE-E101 by `fix_finding_for_check` under the join bd-bigu7 records, GUARDED on the check name so a future second EE-E101 emitter cannot inherit this repair and quarantine the wrong file. Operation `quarantine_by_rename`, outcome `applied`, `status: completed_ok`, `actionCount: 1`, `guidanceOnlyFixerCount: 0`. The malformed config is renamed aside rather than deleted — RULE 1 and the doctor runtime contract both forbid doctor removing a file — and doctor then reports `healthy: true`, posture `ok`, empty `actionable`.
- **Undo:** Applicable now. `--undo <run-id>` restores the damaged baseline, and the fixture asserts `EE-E101` returns afterwards, because `doctor_fixture_corrupt` writes `before.sha256` AFTER the damage, so the digest comparison alone would also be satisfied by a workspace that was never repaired.
- **Oracle:** `doctor_fixture_assert` (the shared repair round trip) plus four assertions it does not make: detection before any repair, the fixer operation BY NAME, undo reporting `actionsUndone > 0`, and `EE-E101` returning after undo.
- **Negative control:** The comment-only baseline config passes `ee search` and a healthy doctor report before it is broken.
- **Pinned sha:** oracle measured 2026-10-14 on an RCH worker against a binary that job built from `.rch-target/debug/ee`. The previous pin was 02524267e, whose behaviour this entry no longer describes.
