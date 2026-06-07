# Dueling-Wizards Observability And No-Silent-Cap Contract

This is the human-facing companion for
`tests/fixtures/contracts/dueling_wizards_observability_no_silent_cap.json`,
enforced by
`tests/contracts/dueling_wizards_observability_no_silent_cap.rs`.

`bd-1n0np.15.5` covers the observability rule for the dueling-wizards
initiative: every new subsystem must emit structured tracing fields, and every
cap, sample, top-N selection, truncation, or abstention must log the count
dropped plus the reason. A green test run must never hide partial coverage.

## Required Subsystems

The manifest tracks the subsystem surfaces that must carry the shared tracing
contract before their runtime implementation can close:

| Subsystem | Owner bead | Purpose |
| --- | --- | --- |
| `evidence_harvester` | `bd-1n0np.2` | Passive outcome attribution and calibration joins. |
| `anchors_freshness` | `bd-1n0np.3` | Code anchors, symbol drift, and freshness penalties. |
| `error_recall` | `bd-1n0np.4` | Memory-backed diagnosis for recurring tool failures. |
| `read_fence` | `bd-1n0np.8` | Generation-consistent reads across derived assets. |
| `write_immune` | `bd-1n0np.8` | Per-source write anomaly detection and quarantine. |
| `gap_honesty` | `bd-1n0np.6` | Blind-spot maps and query-miss clustering. |
| `contradiction_resolution` | `bd-1n0np.7` | Audited contradiction suppression and explanation. |
| `harness_contract` | `bd-1n0np.15.5` | The cross-cutting checker and e2e logging rule. |

## Required Trace Fields

Every subsystem event uses the shared fields from
`docs/observability/tracing_field_convention.md`:

| Field | Requirement |
| --- | --- |
| `workspace_id` | Stable workspace id when a workspace was resolved. |
| `request_id` | Per-invocation request id from CLI entry or harness setup. |
| `bead_id` | Owning implementation bead, or `unassigned` only before ownership is known. |
| `surface` | Stable subsystem id from the manifest. |
| `phase` | One of `input`, `dispatch`, `dependency_check`, `persistence`, `response`. |
| `elapsed_ms` | Measured duration for exit events and sub-operations. |
| `degraded_codes` | Sorted non-empty degraded codes when a response was affected. |

Trace code should use structured fields in `tracing::info!`,
`tracing::debug!`, `tracing::warn!`, or `#[tracing::instrument]`. Do not bury
field names only in the message string.

## No-Silent-Cap Rule

The cap-event vocabulary is:

| Operation | Required event shape |
| --- | --- |
| `truncation` | `cap_kind`, `dropped_count`, `drop_reason`, `cap_limit`, `retained_count`. |
| `sampling` | `cap_kind`, `dropped_count`, `drop_reason`, `cap_limit`, `retained_count`. |
| `top_n` | `cap_kind`, `dropped_count`, `drop_reason`, `cap_limit`, `retained_count`. |
| `abstention` | `cap_kind`, `dropped_count`, `drop_reason`, `cap_limit`, `retained_count`. |

Use `scripts/lib/e2e_harness.sh` `log_drop` in real-binary e2e scripts when a
test observes a cap, sample, top-N omission, truncation, or abstention. Runtime
code should emit the same vocabulary through tracing and machine responses
where the behavior changes the output.

## Cap Event Examples

The manifest includes `capEventExamples` for every cap operation. Each example
must identify a real surface and phase, use the operation name as `cap_kind`,
set a non-zero `dropped_count`, keep `retained_count <= cap_limit`, and provide
a concrete `drop_reason`.

The required examples currently pin these reasons:

| Operation | Example reason |
| --- | --- |
| `truncation` | `token_budget_exceeded` |
| `sampling` | `fixture_sample_limit` |
| `top_n` | `ranked_output_limit` |
| `abstention` | `required_dependency_unavailable` |

These examples are intentionally small, but they prevent future runtime slices
from satisfying the contract with field names alone. A cap event that hides a
zero drop, omits a reason, or reports impossible counters is still silent for
agent debugging purposes.

## Subsystem Coverage Matrix

The manifest also carries a `subsystemCoverageMatrix` row for every required
subsystem. The matrix accounts for each subsystem's shared trace-field count,
cap operation count, cap event field count, source-anchor count, static MUST
coverage, no-silent-cap posture, and runtime proof posture.

The matrix status vocabulary is intentionally narrow:

- `shared_fields_declared`: the subsystem carries the shared structured tracing
  fields from the manifest.
- `no_silent_cap_declared`: the subsystem carries the cap operation and cap
  event field vocabulary.
- `planned_contract_only`: the subsystem is not implemented yet, so source
  anchors are not required.
- `source_anchors_required`: the subsystem is implemented and must carry source
  anchors that exist in the checkout.
- `rch_required_local_invalid`: Cargo-backed runtime proof must run through
  RCH; local Cargo is not accepted for closeout.
- `declared_conformant`: the static manifest contract is complete for this
  subsystem.

## Checker Anchors

The static contract anchors this rule to existing build-independent surfaces:

| Anchor | Role |
| --- | --- |
| `docs/observability/tracing_field_convention.md` | Shared tracing field convention and source pattern. |
| `scripts/check-tracing-fields.sh` | Build-independent tracing paragraph/source checker. |
| `tests/contracts/tracing_paragraph_required.rs` | Beads/TRACING paragraph contract test. |
| `tests/contracts/no_silent_fallback.rs` | Existing no-silent-fallback inventory shape. |
| `scripts/lib/e2e_harness.sh` | E2E helper that exposes `log_drop`. |

## Implementation Rule

When a subsystem moves from planned contract to implemented source:

1. Add structured tracing at the public request path and important cap points.
2. Include the shared trace fields and the no-silent-cap fields in source or
   e2e evidence.
3. Update feature docs and any golden or schema fixtures in the same change.
4. Run static checks, then run Cargo-backed proof only through RCH when remote
   capacity is available.

Local Cargo fallback is not valid proof for this contract.
