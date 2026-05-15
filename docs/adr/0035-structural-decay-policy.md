# ADR 0035: Structural Decay Policy

Status: accepted
Date: 2026-05-15
Bead: bd-mvld.5

## Context

`ee curate` previously evaluated time-to-live disposition with a uniform
age-based decay policy. That was simple, but it ignored the memory graph. A
candidate attached to a load-bearing bridge memory aged at the same rate as a
candidate attached to a peripheral leaf, even though tombstoning the bridge can
disconnect otherwise related knowledge clusters.

The GraphAccretion G5 work adds two deterministic graph signals to the curation
surface:

- Articulation points: memories whose removal increases the number of connected
  components in the memory-link graph.
- Onion layers: shell indices from k-shell peeling, where low layers are
  peripheral and higher layers are deeper in the graph core.

This is a retention-policy change, not only a reporting feature. Future audits
will see different disposition timing for structurally different memories, so
the rationale must be explicit and durable.

## Decision

`ee curate disposition` uses structurally informed decay by default. The
age-based decay for each candidate is multiplied by a deterministic
`structural_multiplier` derived from the target memory's articulation-point
status and onion-layer position.

The default policy is:

```text
onion_normalized = (max_layer - memory.onion_layer) / max_layer
onion_multiplier = 1.0 + (onion_decay_max - 1.0) * onion_normalized
articulation_multiplier = articulation_protection if memory.is_articulation_point else 1.0
structural_multiplier = onion_multiplier * articulation_multiplier
```

The shipped defaults are `onion_decay_max = 3.0` and
`articulation_protection = 0.5`.

Operationally:

- Peripheral memories can age up to 3x faster than the uniform baseline.
- Innermost-shell memories stay near the uniform baseline.
- Articulation points get a protective multiplier, currently halving their
  effective decay.
- Single-shell graphs and missing graph membership use the baseline multiplier
  `1.0`; no structure is inferred when the graph does not supply structure.

The disposition report exposes the resulting `structuralAdjustments[]` block in
default mode, including `memoryId`, `onionLayer`, `maxLayer`,
`isArticulationPoint`, `baseDecay`, `structuralMultiplier`, `adjustedDecay`,
`adjustedTtlThresholdSeconds`, and `rationale`.

`--no-structural-decay` is the explicit escape hatch. It restores the legacy
uniform behavior for that invocation and omits the `structuralAdjustments[]`
block from JSON output. This flag exists for auditability and debugging, not as
the preferred policy.

## Consequences

What becomes easier:

- Curation protects bridge memories without requiring a hand-maintained
  allowlist.
- Peripheral candidates can be retired sooner, reducing stale low-structure
  memory without weakening graph connectivity.
- The retention decision is explainable from persisted memory links and
  deterministic graph algorithms.
- JSON consumers can compare `baseDecay` and `adjustedDecay` directly when
  explaining why a candidate is due or not due.

What becomes harder:

- `ee curate disposition` now depends on the derived memory-link graph for its
  default policy. Missing or degenerate graph structure must degrade to
  baseline behavior instead of inventing structure.
- Uniform age thresholds no longer fully explain disposition timing. Operators
  must inspect `structuralAdjustments[]` when auditing default-mode decisions.
- Tests need both legacy and structural runs so the opt-out path remains a real
  behavior, not a stale compatibility claim.

What becomes intentionally impossible:

- A memory cannot receive a bespoke decay multiplier with no graph-derived
  reason. The multiplier must be derivable from articulation-point status,
  onion-layer position, and the policy constants.
- `ee curate` cannot silently apply structural decay while hiding the adjustment
  details from machine-readable output.

## Rejected Alternatives

- **Hand-tuned per-memory decay multipliers**: Rejected because they are not
  derivable from structure and would require a new manual policy surface to
  audit.
- **Per-trust-class decay rates only**: Rejected because trust class does not
  capture graph connectivity. A low-trust memory can still be the only bridge
  between two useful clusters.
- **Probabilistic decay via Beta posterior**: Rejected for this decision because
  feedback uncertainty and structural importance are separate axes. ADR 0032
  covers Beta posteriors for outcome evidence; this ADR covers graph structure.
- **Hard immunity for articulation points**: Rejected because bridges should be
  protected, not immortal. Manual tombstone and future load-bearing policies can
  still make stronger decisions when evidence justifies them.
- **Always use structural decay with no opt-out**: Rejected because auditors
  need a one-command comparison against the legacy uniform policy while the
  structural policy is still young.

## Verification

The structural policy is gated by focused graph and curate tests:

- `structural_decay_uses_baseline_for_single_shell_graphs` proves degenerate
  single-shell graphs do not invent a structural multiplier.
- `structural_decay_protects_articulation_points` proves articulation points
  receive protective multipliers.
- `curation_disposition_structural_decay_protects_bridge_candidate` runs the
  same fixture in legacy and structural mode. The bridge candidate is planned
  under legacy uniform decay but not due under structural decay; the peripheral
  leaf remains planned under structural decay.
- `curation_disposition_no_structural_decay_keeps_legacy_report_shape` proves
  opt-out mode omits `structuralAdjustments[]`.
- The `curation_structural_adjustments_block` snapshot pins the JSON shape for
  the structural adjustment block.

For `bd-mvld.4`, RCH-only verification passed:

- `cargo test --lib curation_disposition_structural_decay_protects_bridge_candidate -- --nocapture`
- `cargo test --lib structural_decay -- --nocapture`

Future changes to this policy must update this ADR, the snapshot, and the
curation disposition tests in the same change.
