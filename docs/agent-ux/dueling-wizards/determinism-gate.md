# Dueling-Wizards Determinism Gate Contract

This document is the human-facing companion for
`tests/fixtures/contracts/dueling_wizards_determinism_gate.json`, enforced by
`tests/contracts/dueling_wizards_determinism_gate.rs`.

`bd-1n0np.15.2` extends the existing determinism gate to the new JSON surfaces
introduced by the dueling-wizards initiative. The rule is simple: given the
same database, indexes, config, task, and query, machine-facing JSON must be
byte-identical after explicitly documented volatile-field removal, and pack
hashes must reproduce exactly where the surface emits a pack.

## Existing Anchors

The initiative builds on the current determinism infrastructure:

| Anchor | Role |
| --- | --- |
| `scripts/e2e_overhaul/determinism.sh` | Real-binary three-run determinism driver. |
| `tests/determinism_unit.rs` | In-process contract tests for tie-breaks, pack hash reproduction, and DB JSON stability. |
| `scripts/lib/e2e_harness.sh` | Feature e2e harness used by new scripts. |
| `docs/agent-ux/dueling-wizards/surface-contract.md` | New-surface checklist that all planned JSON surfaces share. |

`scripts/e2e_overhaul/determinism.sh` now validates this manifest before it
runs the older J7 real-binary surfaces. That shell check is a contract-presence
gate: it proves the dueling-wizards rows, policy, pack-hash requirements, and
coverage accounting are still coherent, but it does not pretend planned
surfaces have runtime three-run proofs. When an individual surface moves to
`implemented`, its implementation slice must add the real command proof in this
script or a focused companion test.

`scripts/e2e_cross_cutting.sh` also validates the manifest from the
cross-cutting gate. That static shell pass pins the required surface set,
shared assertion vocabularies, three-run/RCH-only policy, pack-hash failure
posture, coverage matrix conformance, and the `impact` memory-anchor volatility
contract. It is intentionally a manifest contract only; runtime determinism
still requires the three-run driver above once a surface exists.

## Required Surfaces

Every row below must eventually be covered by the determinism shell gate or a
companion contract test before the corresponding implementation closes.

| Surface id | Planned surface | Required determinism assertion |
| --- | --- | --- |
| `why_not` | `ee why-not <id> --task <task> --json` | byte-identical JSON across three runs. |
| `harvest` | `ee outcome harvest --dry-run --json` | byte-identical JSON and explicit window inputs. |
| `calibration` | `ee outcome calibration --json` | byte-identical JSON and stable reliability buckets. |
| `impact` | `ee impact <surface> --json` | byte-identical JSON and stable anchor ordering. |
| `error_recall` | `ee diagnose-error --json` | byte-identical JSON after redaction and fingerprint canonicalization. |
| `blind_spots` | `ee blind-spots --json` | byte-identical JSON and stable `blindSpots` ordering. |
| `conflict` | `ee conflict list --json` | byte-identical JSON and stable conflicting-pair ordering (load-bearing weight then `conflictId`). |
| `attest` | `ee attest memory <id> --json` | byte-identical JSON and a deterministic `bundleHash` for a fixed subject + database. |
| `docs_bootstrap` | `ee bootstrap docs --dry-run --json` | byte-identical JSON and a deterministic candidate-id set over a fixed doc tree. |
| `read_fence_consistency` | `pack/search/why` consistency block | byte-identical JSON and stable generation verdicts. |
| `pack_lod` | `ee pack --json` | byte-identical JSON plus reproducible pack hash for the default LOD pack path. |
| `feedback_roi` | `ee feedback roi --json` | byte-identical JSON and stable ROI bucket ordering. |

## Determinism Matrix

The manifest carries a `determinismMatrix` row for each required surface. Each
row mirrors the shared policy and the matching surface entry:

| Matrix field | Required value |
| --- | --- |
| `runCount` | `3`; each runtime proof compares three runs. |
| `canonicalization` | `explicit_volatile_field_removal`; stripped fields must be named first. |
| `stdoutMachineOnly` | `true`; diagnostics go to `stderr_or_artifact`. |
| `runtimeProof` | `rch_only`; Local Cargo fallback is not valid proof. |
| `requiredAssertions` | Same shared assertion set as the surface row. |
| `volatileFields` | Empty unless the surface declares volatility, as `impact` does for anchor timestamps. |
| `packHashExpected` | `true` only for pack-emitting surfaces. |
| `packHashAbsenceFailure` | `true` whenever `packHashExpected` is true. |
| `packHashField` | `data.pack.hash` for pack-emitting surfaces, otherwise `null`. |

The current pack-emitting rows are `read_fence_consistency` and `pack_lod`.
Their absence of `data.pack.hash` is a failed determinism proof, not a skip.

## Surface Coverage Matrix

The fixture also carries a `surfaceCoverageMatrix` section. This is the
accounting view for the determinism gate: one row per required surface, with
counts mirrored from the matching `surfaces` entry and the corresponding
`determinismMatrix` row. The contract checks owner bead count, schema reference
count, required assertion count, pack assertion count, and volatile field count
for every surface.

`determinismStatus=three_run_contract_declared` means the row is covered by the
three-run byte-stability contract, even when the runtime surface is still
planned. `packHashStatus=pack_hash_required` applies only to
`read_fence_consistency` and `pack_lod`; every other row uses `not_applicable`.
`runtimeProofPolicy=rch_required_local_invalid` records that RCH is required
and Local Cargo fallback is invalid proof.

The shared assertion ids are `byte_identical_json`,
`volatile_fields_explicit`, `stable_ordering`,
`stderr_or_artifact_diagnostics`, `pack_hash_reproducible`, and
`pack_hash_absence_is_failure_not_skip`.
Each row currently carries `mustClauses=9`, `tested=9`, `passing=9`,
`divergent=0`, `scoreMilli=1000`, and
`complianceStatus=declared_conformant`. Future additions should make any
determinism gap explicit in that row instead of leaving it implicit in prose.

The `impact` row carries an explicit `memory_anchors` sub-contract because
anchor extraction is the first tier-1 surface likely to emit derived source
metadata. The migration shape comes from
`tests/fixtures/contracts/dueling_wizards_migration_registry.json`; repeated
runs must prove `stable_anchor_value_hash`, `stable_redacted_anchor_value`,
`stable_ordering`, `generation_not_wall_clock`, and
`raw_anchor_value_absent`.

For anchors, hash input material is
`normalized_anchor_value_with_anchor_kind_and_source_class`. Raw anchor values
are excluded, and the only stable value material is the planned
`anchor_value_hash` plus deterministic `redacted_anchor_value`. Generations
come from `workspace_generation_not_wall_clock`; timestamps such as
`created_at` and `updated_at` are volatile fields, not ordering authority.

## Volatile Fields

Volatile fields must be declared centrally before they are stripped. The current
source of truth is `VOLATILE_FIELD_NAMES` in
`scripts/e2e_overhaul/determinism.sh`. Do not hide a nondeterministic field by
adding it to the strip list unless the value has no semantic load and the
reason is documented with the implementation.

## Implementation Rule

When a surface is implemented:

1. Add its command shape to the determinism manifest.
2. Add a three-run assertion in `scripts/e2e_overhaul/determinism.sh` or a
   focused companion test if the surface requires custom setup.
3. Assert byte-identical canonical JSON after explicit volatile-field removal.
4. Assert pack hash reproduction for surfaces that emit `data.pack.hash`.
5. Keep stdout machine-facing and put diagnostics in logs or stderr.
6. Run static checks, then run Cargo-backed verification only through RCH when
   remote capacity is safe.

Local Cargo fallback is not valid proof for this contract.
