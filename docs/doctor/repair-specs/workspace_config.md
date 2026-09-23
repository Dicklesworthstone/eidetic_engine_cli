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

- **Label:** UNCLASSIFIED
- **Severity:** P1. Scored P1 as `pop-config-malformed` in `docs/doctor/failure_mode_scores.jsonl`.
- **Detector:** Not measured by this fixture. The measured sibling `fm-state_files-merge-conflict-markers` shows doctor swallows `config.toml` parse errors.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.
