# Dueling Wizards Why And Pack DNA Signal Contract

Bead: bd-1n0np.23.5
Manifest: tests/fixtures/contracts/dueling_wizards_why_packdna_signals.json
Contract: tests/contracts/dueling_wizards_why_packdna_signals.rs

This contract pins the additive explanation signals that the dueling-wizards
initiative expects in `ee why` and Pack DNA once the underlying feature slices
land. It is a planning gate, not proof that the runtime fields are implemented
today. Its job is to prevent the cross-cutting explanation work from becoming
implicit or scattered across feature-specific beads.

## Authority Points

- `docs/schemas/ee.why.v1.json` owns the stable `ee why` augmentation
  envelope. New fields must be additive and must not rename the existing graph
  fields.
- `docs/schemas/ee.context.pack_dna.v1.json` owns the Pack DNA explanation
  block. New signal fields must preserve the existing `voronoiDominator`,
  `communityOfMass`, `egoSubgraph`, `pprNeighbors`, and `degraded` fields.
- `docs/schemas/ee.why.causal.v1.json` owns causal explanation paths.
- `src/core/why.rs` owns `ee why` assembly.
- `src/graph/pack_dna.rs` owns Pack DNA assembly.

## Required Signals

The manifest requires six signal groups:

- `freshness_symbol_drift`: code-anchored freshness tells an agent whether a
  memory is stale because an anchored symbol changed, disappeared, or could not
  be resolved. `ee why` must surface the drift reason, and Pack DNA must expose
  the same drift as pack-composition context.
- `contradiction_suppressed`: contradiction resolution tells an agent when a
  memory or candidate was suppressed because contradictory evidence won. The
  signal must include source strength and an actionable next inspection path.
- `sentinel_state`: memory sentinels tell an agent whether freshness/validity
  checks last passed, failed, or were unavailable, including `lastVerifiedAt`.
- `task_lens`: task-lens selection tells an agent which lens shaped the pack,
  including a stable lens id and hash.
- `anchor_file_line_provenance`: code anchors tell an agent the redacted
  `file:line` provenance that linked a memory to source, without raw secret
  paths or unreviewed body export.
- `causal_ancestry_path`: causal PPR tells an agent the causal ancestry path
  that made a memory relevant, using the existing `ee.why.causal.v1` path
  vocabulary.

## Signal Coverage Matrix

The manifest also carries a `signalCoverageMatrix` row for each required
signal. Each row accounts for the signal's owner beads, `ee why` fields, Pack
DNA fields, schema references, static MUST assertions, redaction posture,
degraded handling, and runtime proof rule.

The matrix status vocabulary is intentionally narrow:

- `stable_additive`: the signal can extend `ee why` and Pack DNA without
  renaming existing fields.
- `redaction_safe`: the signal must hash or redact sensitive payloads before
  exposing explanation data.
- `degraded_not_silent`: missing upstream feature data must be visible as a
  degraded or unavailable signal.
- `concrete_question` and `concrete_decision`: every signal must answer a
  specific agent question and name the decision it is allowed to influence.
- `rch_required_local_invalid`: runtime proof must use RCH; local Cargo is not
  accepted for implementation closeout.
- `planned_conformant`: the static planning contract is complete, while runtime
  fields still require the owning implementation slices.

## Agent Questions And Decision Impacts

Each signal must answer a concrete agent question and name the decision it is
allowed to influence:

| Signal | Agent question | Decision impact |
| --- | --- | --- |
| `freshness_symbol_drift` | Did source movement make this memory stale? | `rank_down_or_reverify_stale_memory` |
| `contradiction_suppressed` | Was this memory suppressed by stronger contradictory evidence? | `inspect_winning_evidence_before_reusing_memory` |
| `sentinel_state` | Did the latest sentinel check pass for this memory? | `trust_verified_memory_or_schedule_check` |
| `task_lens` | Which task lens shaped this pack selection? | `explain_pack_lens_and_compare_alternatives` |
| `anchor_file_line_provenance` | Which source anchor connected this memory to code? | `jump_to_redacted_source_anchor` |
| `causal_ancestry_path` | Which causal path made this memory relevant? | `inspect_causal_path_before_accepting_relevance` |

These fields keep the Pack DNA extension agent-facing. A future implementation
that adds a schema field but cannot say what question it answers, or which
decision it changes, is not conformant with this contract.

## Compatibility Rules

All fields are additive. Existing `ee why` consumers must still be able to
parse the old graph blocks when the new signals are absent or degraded. Existing
Pack DNA consumers must still be able to inspect dominators, community of mass,
ego subgraph, PPR neighbors, and graph-specific degradations. Missing upstream
feature data must surface as a degraded or unavailable signal, not as a missing
top-level envelope or a renamed existing field.

The signal payloads must be redaction-safe by default. File anchors use
redacted or hashed path fragments where needed; logs and memory bodies do not
leak through these explanation blocks. Every signal must have a concrete
follow-up inspection command, normally `ee why`, `ee pack --explain`, or the
feature-specific command that owns the signal.

Local Cargo fallback is not valid proof for this initiative. Runtime tests for
these fields require RCH-only proof; this contract can be validated with static
manifest, formatting, and anchor checks while RCH is occupied.
