The dueling-wizards verify wiring contract is pinned by
`tests/fixtures/contracts/dueling_wizards_verify_wiring.json` and enforced by
`tests/contracts/dueling_wizards_verify_wiring.rs`.

`bd-1n0np.15.4` owns the rule that every original dueling-wizards feature E2E
script is wired as an ordered `scripts/verify.sh` gate. This document is a
planned-contract guard: it does not claim the future runtime scripts exist yet.
It fixes the expected gate shape so each implementation slice knows the exact
verify obligation before adding a new public surface.

## Required Gate Shape

Each feature E2E gate must:

- be represented in the manifest under `featureE2eScripts`;
- use an ordered, unique verify stage;
- call `run_stage` in `scripts/verify.sh` when implemented;
- source `scripts/lib/e2e_harness.sh`;
- emit `ee.test_event.v1` rows;
- report exit code, elapsed time, and artifact directory evidence;
- fail fast through the verify runner;
- keep Cargo-backed proof behind RCH, never local Cargo fallback.

The machine evidence names are `run_stage`, `exit_code`, `elapsed_ms`,
`artifact_dir`, and `ee.test_event.v1`.

Local Cargo fallback is not valid proof for this contract.

## Verify Gate Matrix

The manifest carries a `verifyGateMatrix` row for every feature listed in
`featureE2eScripts`. Each row must mirror the feature id, order, script,
verify stage, status, expected harness, and required evidence. The matrix also
pins the closeout checklist used when a row moves to `implemented`:

| Requirement | Meaning |
| --- | --- |
| `script_exists` | The planned `scripts/e2e_*.sh` file exists. |
| `sources_harness` | The script sources `scripts/lib/e2e_harness.sh`. |
| `verify_run_stage` | `scripts/verify.sh` calls the script through `run_stage`. |
| `emits_test_event` | The script emits `ee.test_event.v1` rows. |
| `records_exit_code` | The gate records machine-readable exit-code evidence. |
| `records_elapsed_ms` | The gate records elapsed-time evidence. |
| `records_artifact_dir` | The gate records an artifact directory. |
| `rch_only_cargo_proof` | Cargo-backed proof is run through RCH only. |

Rows use `implementedEvidenceMode:
required_when_status_implemented`. Planned rows do not need to create scripts
early, but implemented rows must satisfy the full matrix before closeout.

## Gate Coverage Matrix

`gateCoverageMatrix` is the accounting layer for the planned verify gates. It
keeps one row per `featureE2eScripts` entry and mirrors the gate id, order,
status, script, and verify stage while recording how much of the closeout
checklist is accounted for.

Each row uses `planned_gate_declared` until the corresponding feature script is
implemented. `requiredEvidenceCount` mirrors the `verifyGateMatrix` evidence
list, `implementationRequirementCount` mirrors the closeout checklist, and
`preflightContractCount` mirrors any `preflightContracts` on the feature row.
Rows with preflight contracts use `preflight_contracts_declared`; rows without
them use `not_applicable`.

The coverage counters are explicit: `mustClauses`, `tested`, `passing`,
`divergent`, and `scoreMilli` must show full conformance before a row can be
treated as complete. `runtimeProofPolicy: rch_required_local_invalid` records
that Cargo-backed proof must run through RCH and that local Cargo output is not
valid proof. `eventLogStatus: ee_test_event_required` pins the structured event
log obligation, and `complianceStatus: declared_conformant` means the planned
gate declaration satisfies this static contract.

## Original Feature Scripts

| Surface | Bead | Planned script |
| --- | --- | --- |
| `why_not` | `bd-1n0np.1` | `scripts/e2e_why_not.sh` |
| `evidence_harvester` | `bd-1n0np.2` | `scripts/e2e_evidence_harvester.sh` |
| `anchors_freshness` | `bd-1n0np.3` | `scripts/e2e_anchors_freshness.sh` |
| `error_recall` | `bd-1n0np.4` | `scripts/e2e_error_recall.sh` |
| `lod_packing` | `bd-1n0np.5` | `scripts/e2e_lod_packing.sh` |
| `gap_honesty` | `bd-1n0np.6` | `scripts/e2e_gap_honesty.sh` |
| `contradiction_resolution` | `bd-1n0np.7` | `scripts/e2e_contradiction.sh` |
| `store_integrity` | `bd-1n0np.8` | `scripts/e2e_store_integrity.sh` |
| `provenance_reverification` | `bd-1n0np.9` | `scripts/e2e_provenance_reverification.sh` |
| `house_rules` | `bd-1n0np.10` | `scripts/e2e_house_rules.sh` |
| `docs_bootstrap` | `bd-1n0np.11` | `scripts/e2e_docs_bootstrap.sh` |
| `typed_kinds` | `bd-1n0np.12` | `scripts/e2e_typed_kinds.sh` |
| `feedback_gated` | `bd-1n0np.13` | `scripts/e2e_feedback_gated.sh` |

Later dueling-wizards feature epics must append to the manifest and receive the
same ordered verify-stage treatment. The append-only rule prevents a new
feature from relying on unit tests alone while skipping the real-binary E2E
gate.

The `anchors_freshness` row also carries `preflightContracts` for the
memory-anchor planning surface. It may not move to `implemented` without
the cross-cutting contracts named in the manifest:
`dueling_wizards_migration_registry`, `dueling_wizards_backup_coverage`,
`dueling_wizards_determinism_gate`, and `dueling_wizards_mesh_redaction`.

## Verify Anchors

`scripts/verify.sh` is the only default readiness runner. Implemented feature
scripts must be wired there with `run_stage`, next to existing E2E gates such as
`Basic E2E Scripts`, `Replay Lab Smoke E2E`, `Advanced E2E Scripts`, and
`Boundary Migration Scripts`.

`scripts/lib/e2e_harness.sh` is the common feature harness. Scripts use its
`harness_init`, assertion helpers, `log_drop`, and `harness_summary` behavior so
future failures carry structured artifacts instead of unstructured shell noise.

`scripts/e2e_event_contract_radar.sh` remains the static scanner for
`ee.e2e_event_contract_radar.v1` event-contract gaps. It is not a replacement
for running the real-binary E2E scripts through `scripts/verify.sh`; it is a
cheap preflight that catches missing structured logging before the heavier gates.

## Closeout Rule

When a feature implementation lands, update its manifest row from `planned` to
`implemented`, ensure the script exists and sources `scripts/lib/e2e_harness.sh`,
wire it into `scripts/verify.sh`, and prove the Cargo-backed parts only through
RCH. If RCH blocks before Cargo, preserve the exact blocker string and leave the
source verdict separate from proof posture.
