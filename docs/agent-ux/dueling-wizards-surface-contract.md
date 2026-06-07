# Dueling-Wizards Surface Contract

This checklist is the human-facing companion to
`tests/fixtures/contracts/dueling_wizards_surface_contract.json`. It covers the
new surfaces planned by `bd-1n0np` and is enforced by
`tests/contracts/dueling_wizards_surface_contract.rs`.

The goal of `bd-1n0np.15.3` is not to make future surfaces pass by default. It
is to make each new command, JSON schema, degraded code, and `EE_*` variable
declare its contract work before implementation starts, then fail CI once a
surface is marked `implemented` without the required artifacts.

## Required Artifacts

Every manifest entry must carry this full checklist:

| Artifact | Required evidence |
| --- | --- |
| `capabilities` | The surface is visible in capability/status reporting or explicitly listed as unavailable. |
| `agent_docs` | Agent-facing docs describe when agents should use the surface. |
| `robot_docs` | Robot/JSON consumers have stable field guidance. |
| `help_prelude` | The CLI help path advertises the surface and its required flags. |
| `json_schema` | `docs/schemas/*.json` exists for each machine-facing payload. |
| `schema_drift` | The schema appears in the schema drift inventory or a companion schema test. |
| `failure_mode_fixture` | Each new degraded code has `tests/fixtures/failure_modes/<code>.json`. |
| `degraded_taxonomy` | Each new degraded code appears in the degraded-code taxonomy. |
| `env_registry` | Each new `EE_*` variable is registered in `src/config/env_registry.rs`. |
| `determinism` | Repeated runs over the same fixture produce byte-identical JSON or an explicit deterministic hash. |
| `e2e_harness` | Real-binary e2e coverage uses `scripts/lib/e2e_harness.sh` and logs `ee.test_event.v1`. |

If an artifact category does not apply yet, keep the category in the manifest
and leave the corresponding list empty. Do not delete the category. Empty lists
mean "no artifact exists yet"; missing categories mean the checklist drifted.

## Coverage Matrix

The manifest also carries a `coverageMatrix` array. Each row is a conformance
accounting record for one required artifact category. The contract test requires
one row per required artifact, at least one covered MUST clause, zero divergent
clauses, and a `scoreMilli` of at least `950`.

| Spec section | MUST clauses | SHOULD clauses | Tested | Passing | Divergent | Score |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `capabilities` | 1 | 0 | 1 | 1 | 0 | 1000 |
| `agent_docs` | 1 | 0 | 1 | 1 | 0 | 1000 |
| `robot_docs` | 1 | 0 | 1 | 1 | 0 | 1000 |
| `help_prelude` | 1 | 0 | 1 | 1 | 0 | 1000 |
| `json_schema` | 1 | 0 | 1 | 1 | 0 | 1000 |
| `schema_drift` | 1 | 0 | 1 | 1 | 0 | 1000 |
| `failure_mode_fixture` | 1 | 0 | 1 | 1 | 0 | 1000 |
| `degraded_taxonomy` | 1 | 0 | 1 | 1 | 0 | 1000 |
| `env_registry` | 1 | 0 | 1 | 1 | 0 | 1000 |
| `determinism` | 1 | 0 | 1 | 1 | 0 | 1000 |
| `e2e_harness` | 1 | 0 | 1 | 1 | 0 | 1000 |

## Surface Inventory

The machine-readable inventory is authoritative. This table is only a quick
map for humans.

| ID | Bead | Status | Purpose |
| --- | --- | --- | --- |
| `why_not` | `bd-1n0np.1` | `in_progress` | Counterfactual exclusion explanations for pack/search omissions. |
| `evidence_harvester` | `bd-1n0np.2` | `planned` | Passive outcome attribution, calibration, and derived evidence joins. |
| `anchors_freshness` | `bd-1n0np.3` | `planned` | Code anchors, freshness penalties, and surface memory maps. |
| `error_recall` | `bd-1n0np.4` | `planned` | Memory-backed diagnosis for recurring tool and build failures. |
| `lod_packing` | `bd-1n0np.5` | `planned` | Telescoping level-of-detail context packing. |
| `gap_honesty` | `bd-1n0np.6` | `planned` | Blind-spot maps and query-miss clustering. |
| `contradiction_resolution` | `bd-1n0np.7` | `planned` | Audited contradiction detection and pack guards. |
| `read_fence` | `bd-1n0np.8` | `planned` | Multi-agent read fences and write-immune store checks. |
| `provenance_reverification` | `bd-1n0np.9` | `planned` | Revalidating cited evidence over time. |
| `house_rules` | `bd-1n0np.10` | `in_progress` | Cross-workspace global memory and house-rule retrieval. |
| `docs_bootstrap` | `bd-1n0np.11` | `planned` | Compiling repo docs into initial memories. |
| `typed_memory_kinds` | `bd-1n0np.12` | `planned` | Lightweight per-kind schemas and extraction hints. |
| `feedback_learning` | `bd-1n0np.13` | `planned` | Token ROI, regime-shift, and calibration-honesty feedback. |
| `rejected_ideas` | `bd-1n0np.14` | `planned` | Durable register of rejected ideas and rationale. |
| `harness_contract` | `bd-1n0np.15` | `implemented` | Shared e2e harness and this new-surface checklist gate. |
| `memory_sentinels` | `bd-1n0np.16` | `planned` | Declarative per-memory validity checks. |
| `task_lens` | `bd-1n0np.17` | `planned` | Named pack/search policies that are inspectable and reusable. |
| `trauma_guard_loop` | `bd-1n0np.18` | `planned` | Bypass-evidence feedback into trauma-guard precision. |
| `causal_ppr` | `bd-1n0np.19` | `planned` | Causal-ancestry PPR pre-warming for upstream-task lessons. |
| `bridge_exemption` | `bd-1n0np.20` | `planned` | Protecting rare disaster-recovery memories from decay. |
| `memory_sandbox` | `bd-1n0np.21` | `planned` | Simulating memory changes before durable mutation. |
| `attestation_bundles` | `bd-1n0np.22` | `planned` | Chain-of-custody manifests for evidence bundles. |
| `cross_cutting_foundations` | `bd-1n0np.23` | `planned` | Migrations, backup, ingestion security, mesh, and why-enrichment glue. |

### `anchors_freshness` Anchor Contract

The anchor substrate is the first surface where ambiguous extraction can poison
future retrieval, graph, and freshness behavior. Its manifest entry therefore
has an `anchorContract` block that future implementation slices must preserve.

The planned `allowedKinds` are `path`, `symbol`, `command`, `env_var`, `schema`,
`degraded_code`, `dependency`, and `config_key`. Extraction sources are limited
to `explicit`, `remember`, `cass_import`, `curate_apply`, and `index_rebuild`.
The precision policy is `precision_first_no_adversarial_prose`: an extractor
must prefer missing an anchor over inventing one from prose that only looks like
a path, command, schema, or environment variable.

Anchor values are not raw shareable payloads. The redaction policy is
`hash_or_redact_anchor_values_keep_kind_and_line`, so robot outputs may preserve
the anchor kind and useful line number while hashing or redacting sensitive
paths, symbols, source hashes, and values. Freshness is
`rank_down_resolved_symbol_drift_never_tombstone`: exact resolved symbol drift
can demote a memory and emit degraded evidence, but it must not silently delete,
tombstone, or hide the memory. Missing anchor data is
`degraded_not_silent`.

Required follow-up commands are:

- `ee memory anchors <memory-id> --json`
- `ee impact <surface> --json`
- `ee pack <task> --surface <hint> --json`

Current implementation status: `ee impact <surface> --json` is the live
read-only lookup surface for anchored paths, symbols, commands, environment
variables, schemas, degraded codes, dependencies, and config keys. `ee pack
<task> --surface <hint> --json` remains a follow-up surface until the pack
surface hint slice is implemented.

## Implementation Rule

When a surface moves to `implemented`, update the manifest in the same change:

1. Set `status` to `implemented`.
2. Fill `plannedCommands`, `schemas`, `degradedCodes`, and `envVars` with the
   concrete names used by the source.
3. Add every current evidence path to `implementedArtifacts`.
4. Add or update the e2e script to use `scripts/lib/e2e_harness.sh`.
5. Run the static checks and the RCH-only Cargo contract proof.

Local Cargo fallback is not valid proof for this checklist.
