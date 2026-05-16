# GraphAccretion Rollout Plan

Tracking bead: `bd-bife.8`

This plan phases the GraphAccretion work so the framework can land before the
user-facing graph features are enabled. The objective is partial shippability:
each phase must have a clear verification gate, honest degraded output, and no
requirement that all ten graph features finish at once.

## Rollout Principles

- Keep `ee` CLI-first and local-first; graph features enrich existing commands
  but do not turn `ee` into an agent scheduler.
- Keep graph state derived. FrankenSQLite/SQLModel tables remain authoritative;
  graph snapshots, witnesses, and caches are rebuildable assets.
- Keep outputs additive. Existing `ee context`, `ee why`, `ee status`,
  `ee curate`, and `ee health` JSON fields must not be renamed or removed.
- Keep disabled features honest. A disabled graph feature leaves its response
  field present where the schema requires it and emits a degraded sentinel.
- Keep verification remote-only for Cargo gates in this checkout; local
  non-Cargo checks can validate docs, JSON, and shell-only fixtures.

## Phase Alpha: Foundation

Scope:

- F1 multi-graph snapshot framework.
- F2 algorithm wrapper conventions, including budgets, sampling, witnesses,
  and cache-key rules.
- F4 graph determinism harness and schema/snapshot governance.

No new user-facing ranking or diagnostic behavior should be enabled in this
phase. The only intended user-visible changes are additive schemas, docs, and
framework surfaces that either report empty results or existing graph behavior.

Exit gate:

- Snapshot builders for the foundation graph types are deterministic.
- Algorithm wrappers emit complexity witnesses or explicit degraded entries.
- Existing pagerank or centrality behavior is byte-identical where used as the
  proof point.
- Graph schema registry and snapshots are present.

## Phase Beta: Insights Scaffold

Scope:

- F3 `ee insights` command scaffold.
- `ee.insights.v1` schema and degraded-signal envelope.
- Section registry with deterministic ordering.

Sections may be empty in this phase, but they must be explicit. Missing
implementation is represented as degraded output, not by omitting a documented
section without explanation.

Exit gate:

- `ee insights --json` returns a valid empty bundle.
- `ee insights --section <name>` is deterministic for all registered sections.
- Unknown sections return a clear usage/configuration error.
- Stubbed sections use an honest disabled or not-yet-implemented sentinel.

## Phase Gamma: First Value Features

Scope:

- G1 personalized PageRank for context ranking.
- G4 k-truss and contradiction-cluster health diagnostics.
- G5 articulation points and onion-layer structural decay.

These features exercise different algorithm classes and produce immediate
agent value: better context ordering, structural health signals, and safer
curation/decay decisions.

Exit gate:

- Each feature has unit tests, golden output, and deterministic e2e coverage.
- Feature-disabled mode is tested for each user-facing surface.
- `ee context`, `ee health`, and `ee curate` stay additive relative to their
  pre-GraphAccretion JSON shapes.

## Phase Delta: Composite Surfaces

Scope:

- G2 Pack DNA, composed from neighborhood, community, ego graph, and PPR data.
- G8 knowledge skyline, composed from community, onion, age, trust, k-truss,
  and PPR signals.

These surfaces should not introduce new independent graph truth. They combine
cached feature outputs into compact agent-readable explanations.

Exit gate:

- Pack DNA and skyline schemas include examples and degraded contracts.
- Composite output explains which component signals were present, stale, or
  unavailable.
- Missing component metrics degrade the composite instead of forcing a full
  command failure.

## Phase Epsilon: Deep Math Features

Scope:

- G3 causal explanation.
- G6 Gomory-Hu proximity.
- G7 dominance frontiers over revision DAGs.
- G9 rule-provenance bipartite load-bearing scores.
- G10 HITS hubs/authorities and query profiles.

These can ship independently once the foundation and scaffold contracts are in
place. They should use the same wrapper, witness, feature-flag, and degraded
output conventions established in earlier phases.

Exit gate:

- Each feature has deterministic fixtures and a documented abstention sentinel.
- Expensive algorithms have budget and sampling behavior recorded in witnesses.
- `ee why` and `ee insights` expose graph badges without hiding provenance.

## Feature Flags

GraphAccretion feature rollout is controlled by config keys, not Cargo feature
flags, once the binary contains the implementation. Cargo features still govern
compiled dependencies, but runtime rollout uses graph config.

Registered keys:

| Feature | Config key | Built-in default | Enabled phase |
| --- | --- | --- | --- |
| Personalized PageRank | `graph.feature.ppr.enabled` | `false` | Phase gamma |
| Pack DNA | `graph.feature.pack_dna.enabled` | `false` | Phase delta |
| Causal explanation | `graph.feature.causal_explain.enabled` | `false` | Phase epsilon |
| Structural health | `graph.feature.structural_health.enabled` | `false` | Phase gamma |
| Structural decay | `graph.feature.structural_decay.enabled` | `false` | Phase gamma |
| Proximity | `graph.feature.proximity.enabled` | `false` | Phase epsilon |
| Revision dominance | `graph.feature.revision_dominance.enabled` | `false` | Phase epsilon |
| Skyline | `graph.feature.skyline.enabled` | `false` | Phase delta |
| Load-bearing provenance | `graph.feature.load_bearing.enabled` | `false` | Phase epsilon |
| HITS profiles | `graph.feature.hits_profiles.enabled` | `false` | Phase epsilon |

The built-in defaults above match `built_in_config()` and
`docs/configuration/graph.md`: runtime graph features default off until a
workspace explicitly opts in. This keeps partially landed surfaces honest by
forcing disabled-mode evidence instead of placeholder graph data.

Disabled behavior:

- The field required by a public schema remains present when practical.
- The command or section emits a degraded entry with a stable code, severity,
  message, and repair/config hint.
- Disabled output must be deterministic and small enough for agent parsing.
- A feature flag must not silently remove a documented command section.

## Disabled-Feature Acceptance Matrix

Each runtime rollout flag has two jobs: suppress the graph-derived behavior for
its owning surface, and emit `graph_feature_disabled` in the response that would
otherwise have used that behavior. These checks are separate from Cargo
feature-gating. A binary can include the code while a workspace config keeps the
feature disabled.

| Config key | Owning user-facing surface | Disabled-mode expectation | Proof artifact |
| --- | --- | --- | --- |
| `graph.feature.ppr.enabled` | `ee context` pack ranking | PPR fields remain deterministic but contribute no rank boost; response records `graph_feature_disabled` for PPR ranking. | Context integration test with flag `false` and `true` over the same seeded workspace. |
| `graph.feature.pack_dna.enabled` | `ee context --explain` / Pack DNA block | Pack DNA block is absent or explicitly degraded according to `ee.context.pack_dna.v1`; no stale Pack DNA values are emitted. | Pack DNA smoke/golden pair covering disabled and enabled output. |
| `graph.feature.causal_explain.enabled` | `ee why --causal-explain` and `ee insights --section causalBottlenecks` | Causal sections stay present as empty/degraded contract objects and emit `graph_feature_disabled`. | Why/insights integration test with a seeded causal fixture. |
| `graph.feature.structural_health.enabled` | `ee health --json` structural graph block | Structural health fields stay stable and report disabled rather than silently falling back to ordinary health. | Health contract test over the structural fixture with the flag disabled. |
| `graph.feature.structural_decay.enabled` | `ee curate` structural decay adjustments | Curation output omits graph decay adjustments and emits a disabled degraded entry tied to the adjustment block. | Curate unit/golden test over an onion-layer fixture. |
| `graph.feature.proximity.enabled` | `ee proximity` and context `proximityToSeed` | `ee proximity` returns a disabled/degraded response; context candidates do not receive proximity boosts. | Proximity CLI test plus context-pack comparison. |
| `graph.feature.revision_dominance.enabled` | `ee memory revise --dry-run` and `ee why` revision lineage | Impact/lineage blocks are empty or disabled-degraded, never populated from stale dominance data. | Revision-chain integration test with disabled flag. |
| `graph.feature.skyline.enabled` | `ee status` / `ee insights --section skyline` | Skyline summary remains schema-valid and reports disabled instead of placeholder scores. | Status/insights snapshot with disabled flag. |
| `graph.feature.load_bearing.enabled` | Rule-provenance / load-bearing insights | Load-bearing scores are suppressed and the section emits `graph_feature_disabled`. | Bipartite fixture integration or insight-section contract test. |
| `graph.feature.hits_profiles.enabled` | HITS query profiles and `ee insights --section hubs` / `authorities` | Profile boosts are suppressed; HITS profile insights emit `graph_feature_disabled` while base `ee graph hits` remains governed by ordinary graph availability instead of this profile rollout flag. | HITS profile integration test or `ee insights --section hubs` / `authorities` contract coverage, plus wrapper unit coverage. |

Closeout for `bd-bife.8` requires at least one executable proof per row. If a
downstream G-feature bead has not landed the owning surface yet, that row stays
open and the test belongs with that downstream bead.

## Remaining Work For `bd-bife.8`

This document satisfies the rollout-plan artifact. The typed config registry
now knows the ten `graph.feature.*.enabled` keys, including default values and
`ee config get/set` round-tripping, but the bead is still not closed by this
alone. Remaining acceptance work:

- Keep disabled-feature behavior implemented for every affected surface in the
  matrix above. The HITS row is scoped to profile-driven context/insights
  surfaces; base `ee graph hits` remains a graph-availability surface, not a
  `graph.feature.hits_profiles.enabled` consumer.
- Add or preserve tests proving disabled features suppress output and emit
  degraded sentinels, using the matrix above as the row-level checklist.
- Run the required Cargo gates remotely through RCH after the topology blocker
  is resolved.
