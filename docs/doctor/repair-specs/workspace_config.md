# workspace_config repair specs

Repair specs for the `workspace_config` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-workspace_config-nested-ee-markers

- **Label:** UNCLASSIFIED
- **Severity:** P1. Not in the scored population: nested `.ee` markers affect workspace resolution before doctor runs.
- **Detector:** Not measured. The fixture is marker-only.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.

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
