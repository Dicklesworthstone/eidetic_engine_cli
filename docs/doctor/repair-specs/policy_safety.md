# policy_safety repair specs

Repair specs for the `policy_safety` fixtures under `tests/doctor_fixtures/`.
Labels, fields and the coverage rule are defined in
[`docs/doctor/README.md`](../README.md).

## fm-policy_safety-trauma-guard-policy-denied-exit-7

- **Label:** UNCLASSIFIED
- **Severity:** P1. Not in the scored population: policy denials are command outcomes, not doctor-read state.
- **Detector:** Not measured. The fixture is marker-only.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.

## fm-policy_safety-redaction-class-coverage-gap

- **Label:** UNCLASSIFIED
- **Severity:** P1. Not in the scored population: redaction coverage is not doctor-read state.
- **Detector:** Not measured. The fixture is marker-only.
- **Real trigger:** Not yet built.
- **Repair:** Not classified.
- **Undo:** Not classified.
- **Oracle:** None yet. `assert.sh` checks only the marker.
- **Negative control:** None yet.
- **Pinned sha:** None. Marker-only; no binary measured.
