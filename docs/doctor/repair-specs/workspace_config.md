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
- **Pinned sha:** fa6967499, measured on its stamped release build on vmi1227854 (corrupt 0, assert 0: "pinned gap confirmed"; with the child's `.ee` moved aside the witness fails with "witness lost").

## fm-workspace_config-config-toml-malformed

- **Label:** NOT-DETECTED (pinned gap; NOT coverage)
- **Severity:** P1. Scored P1 as `pop-config-malformed` in `docs/doctor/failure_mode_scores.jsonl`.
- **Detector:** None. Doctor swallows `config.toml` parse errors (`workspace_config` returns `ConfigFile::parse(..).ok()`), so a workspace whose config cannot be parsed reports healthy. The config quarantine fixer exists but is never dispatched.
- **Real trigger:** A valid comment-only `.ee/config.toml` that `ee search` accepts is moved into `.fixture_baseline/` and replaced by the same content plus invalid TOML (an unclosed `[search` table header and a key without a value). `ee search` then fails with an `ee.error.v2` envelope whose code is `configuration`.
- **Repair:** None. Doctor finds nothing, so `--fix` dispatches nothing.
- **Undo:** Not applicable. The config file is unchanged by `--fix`.
- **Oracle:** `doctor_fixture_assert_pinned_gap` on `.ee/config.toml`: doctor healthy with an empty `actionable`, `--fix` reports 0 actions, and the config bytes are unchanged. The witness is `ee search` failing with `configuration`.
- **Negative control:** The comment-only baseline config passes `ee search` and a healthy doctor report before it is broken.
- **Pinned sha:** 02524267e, measured on its stamped release build on hz3.
