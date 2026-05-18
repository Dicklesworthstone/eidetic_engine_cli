# Degraded aggregation emitter inventory

> **What this file is:** the agent-facing inventory of public
> `data.degraded[]` emitters, classified by whether the response
> renderer routes them through the aggregator at
> `src/core/degraded_aggregation.rs::aggregate_degraded_entries`.
>
> **Tracked by:** [`bd-2kj2x.1`](../README.md) (audit closure of
> `bd-2kj2x` "wire aggregate_degraded helper into all response
> renderers"), parent epic
> [`bd-bife.27`](../README.md).
>
> **How to update:** when you add a new public `degraded[]` emitter,
> add the source label to the "Aggregation source labels" table of
> [`degraded_code_taxonomy.md`](degraded_code_taxonomy.md) AND, if the
> emission carries evidence-rich fields that AggregatedDegradation
> would erase, also list the surface here under "Evidence-rich exempt
> emitters". The contract test
> `tests/degraded_aggregation_emitter_inventory.rs` keeps the literal
> source labels in `src/` in sync with the taxonomy table; the
> exempt-emitter list below is documentation-only because the
> aggregator is not on those code paths.

## Three categories of public `degraded[]` emitters

Every public response that emits a `data.degraded[]` array falls into
one of three categories:

| Category | Routed through aggregator | Examples |
|---|---|---|
| **Aggregated** | Yes. Same code from multiple emitters collapses into one row with `sources[]`, severity escalates, repair hint inherits from the highest-severity source. | `ee context`, `ee search`, `ee insights`, `ee status`, `ee doctor`, most renderers under `src/output/mod.rs` and `src/cli/mod.rs`. |
| **Evidence-rich exempt** | No. Per-entry fields beyond `code`/`severity`/`message`/`repair` are load-bearing for the agent (cycle members, lock paths, next-action remediation, dropped IDs) and would be erased by `AggregatedDegradation`. | `ee why --causal-explain` `causalExplanation.degraded[]`; selected pack-replay freshness streams. |
| **Intentionally empty / build-time** | N/A. The surface either always emits an empty array or only emits build-time markers that belong in `capabilities.unimplemented[]` per the E5 split (see [`degraded_code_taxonomy.md`](degraded_code_taxonomy.md#categorization-rules-canonical)). | `lexical_unavailable`, `mcp_feature_disabled`, and similar build-time codes. |

## Evidence-rich exempt emitters (deliberately not aggregated)

Each exempt emitter loses semantic information when routed through
`AggregatedDegradation`, which keeps only `code`, `severity`,
`message`, `repair`, and `sources`. The surfaces below are documented
exemptions; future emitters that carry richer evidence fields should
be added here in the same commit that introduces them.

### `ee why --causal-explain` — `causalExplanation.degraded[]`

- **File:** `src/cli/mod.rs` (`why_causal_explanation_json`)
- **Extra fields:** `cycleMembers` (the memory IDs in the detected
  cycle), `graph_causal_explanation_unavailable` sentinel with
  remediation prose.
- **Why exempt:** the cycle-members array is the only practical way
  for an agent to act on the degradation; folding it into a generic
  `repair` string would force the agent to re-derive it from
  follow-up commands.

### `ee causal trace`, `ee causal estimate`, `ee causal compare`, `ee causal promote-plan` — top-level `degraded[]`

- **File:** `src/core/causal.rs` (`aggregate_causal_degraded_entries`)
- **Status:** these renderers DO call `aggregate_degraded_entries`
  with source labels `causal_trace`, `causal_estimate`,
  `causal_compare`, and `causal_promote_plan`. The base contract
  applies; this row is here only to disambiguate from the
  `causalExplanation` exemption above which lives inside a different
  response shape.

### Selected pack-replay freshness streams

- **File:** `src/output/streaming.rs` (`aggregate_pack_stream_degraded`)
- **Status:** stream-frame `degraded` entries DO route through the
  aggregator. The exemption is only the embedded per-evidence
  freshness rows inside `pack_replay_summary.json` which preserve
  `evidence_freshness_state` / `evidence_revision` fields. Those
  fields are diagnostic and never aggregate-able by `code`.

## How the audit stays in sync

Two contract tests keep this inventory honest:

1. `tests/degraded_aggregation_emitter_inventory.rs` walks `src/` for
   every literal `DegradationAggregationInput::new("LABEL", ...)`
   call site, asserts each `LABEL` is snake-case ASCII, and asserts
   each `LABEL` is documented in the "Aggregation source labels"
   table of [`degraded_code_taxonomy.md`](degraded_code_taxonomy.md).
   Variable-passed source labels (e.g. `source: &str` parameters in
   `src/core/search.rs`, `src/core/why.rs`, and several
   `src/cli/mod.rs` helpers) are NOT caught by the static walk; per
   the test header note, each such call site is pinned by its own
   renderer-level unit test landed alongside the parent `bd-2kj2x`
   slice that wired it (see `tests/renderer_parity_matrix.rs` and the
   per-surface unit tests under `src/cli/mod.rs` /
   `src/output/mod.rs`).

2. `tests/degraded_aggregation_worst_case.rs` drives the aggregator
   with a single mixed input that triggers ALL of the documented
   aggregation rules at once: duplicate-code collapse, severity
   escalation, alphabetical source-list sorting, deterministic JSON
   output, and truncation-trailer passthrough with the dropped codes
   carried in `trailer.sources`. The inline tests in
   `src/core/degraded_aggregation.rs` each cover one rule in isolation
   under a narrow scenario; the worst-case test catches regressions
   that the per-rule tests might miss by happening to hit different
   code paths.

## Process: adding a new degraded emitter

When wiring a new response renderer that emits `degraded[]`:

1. Decide which category fits (aggregated / evidence-rich exempt /
   build-time).
2. If aggregated: pick a stable snake-case source label, route the
   per-call entries through `aggregate_degraded_entries(...)` (or the
   convenience `aggregate_degraded(...)` for `&[DegradationReport]`),
   add the label as a new row in the "Aggregation source labels"
   table of `degraded_code_taxonomy.md`, and land a unit test under
   the same renderer that asserts the literal label travels through
   to the response JSON.
3. If evidence-rich exempt: add a row to "Evidence-rich exempt
   emitters" above with the file, the extra fields, and a one-sentence
   "why exempt" justification.
4. If build-time: add the code to the "build_time" inventory in
   `degraded_code_taxonomy.md` and ensure it surfaces through
   `capabilities.unimplemented[]` per the E5 split.
5. If the code is new, also add a fixture under
   `tests/fixtures/failure_modes/<code>.json` so the J6 catalog
   validator keeps the documentation in sync with the emission.

The contract test will catch (1)/(2) drift; (3)/(4)/(5) drift is
caught by the existing failure-mode fixture and capabilities tests.

## Cross-references

- `src/core/degraded_aggregation.rs` — the aggregator itself, with
  inline unit tests for each rule.
- [`degraded_code_taxonomy.md`](degraded_code_taxonomy.md) — code
  inventory plus the "Aggregation source labels" table this contract
  test parses.
- [`tests/fixtures/failure_modes/SCHEMA.md`](../tests/fixtures/failure_modes/SCHEMA.md) —
  per-code fixture catalog and severity vocabulary.
- [`AGENTS.md`](../AGENTS.md) "Response envelope contract" section —
  the agent-facing `data.degraded[]` invariant.
