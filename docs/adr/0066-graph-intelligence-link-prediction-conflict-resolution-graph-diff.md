# ADR 0066: Graph Intelligence — Link Prediction, Conflict Resolution, Graph Diff

Status: proposed
Date: 2026-06-10
Bead: bd-3a1op.1 (epic bd-3a1op, 2026-06 idea-wizard wave)

## Context

Links turn isolated facts into navigable knowledge, but today they exist
only when an agent explicitly creates them or remember-time auto-linking
fires. The conflict surface (`ee conflict list/explain/cluster`) is
read-only — a report, not a workflow. And graph snapshots describe the
present with no view of structural change over time. This ADR fixes the
contracts for three accretive surfaces: `ee graph suggest-links`
(deterministic link prediction emitting curation candidates),
`ee conflict resolve` (the audited write-half of the conflict surface), and
`ee graph diff` (temporal structural diff). Fact-checked inputs (2026-06-10):
fnx-algorithms already ships `adamic_adar_index`, `jaccard_coefficient`,
`common_neighbors`, and `preferential_attachment`; the `graph_snapshots`
CHECK constraint uses the family identifiers `memory_links`,
`causal_evidence`, `revision_dag`, `rule_provenance`,
`contradiction_subgraph`, `session_graph`, `procedure_graph`,
`evidence_graph`, `composite`; decision-kind typed fields are `options`,
`chosen`, `rationale`, `supersedes`.

## Decision

### 1. Predictor suite and blended scoring (`ee graph suggest-links`)

- **Candidate generation is bounded**: only unlinked pairs that share at
  least one graph neighbor, at least one tag, or a retrieval-affinity edge
  are scored. Per-node neighbor lists are capped at the top 64 by edge
  weight, bounding worst-case cost at O(Σ deg²) over capped lists — never
  O(n²) over the corpus (a planted-trap fixture enforces this in tests).
- **Signals** (graph signals via fnx, no hand-rolled algorithms):
  - `aa` — Adamic-Adar via `fnx_algorithms::adamic_adar_index` over the
    undirected `memory_links` snapshot.
  - `jaccard_tags` — |tags_a ∩ tags_b| / |tags_a ∪ tags_b| (plain set math
    over memory tags; not a graph metric).
  - `ppr` — symmetrized personalized PageRank affinity
    (ppr_a(b) + ppr_b(a)) read through the existing `ppr.rs` cache with its
    seeded determinism and sampling witnesses.
  - `affinity` — normalized decayed co-occurrence weight from the
    retrieval-affinity projection (§2); honestly omitted with
    `retrieval_affinity_cold` when that snapshot does not exist.
  - `pa` — preferential attachment via fnx (free fourth signal; lowest
    weight — it is a popularity prior, not evidence of relatedness).
- **Blend**: per-signal min-max normalization over the candidate batch
  (epsilon-guarded), then
  `S = 0.35·aa + 0.20·jaccard_tags + 0.25·ppr + 0.15·affinity + 0.05·pa`,
  weights in config `[graph.suggest]`. Deterministic ordering: score desc,
  then pair id. Every suggestion carries the raw per-signal values and a
  one-line reason — an unexplained suggestion is worthless to a reviewer.
- **Suggestions are TYPED**: `suggestedRelation ∈ {related, supports,
  contradicts}`. The `contradicts` type fires only when content similarity
  is high AND the polarity heuristic (shared with the ask pipeline's
  negation detection) detects opposition; precision over recall applies
  doubly — thresholds live in `[graph.suggest]` and are documented with the
  formula. `--propose` maps `contradicts` suggestions to
  contradiction-review curation candidates (feeding `ee conflict`), and
  `related`/`supports` to link-creation candidates. Default is a read-only
  report; nothing is ever auto-applied; re-proposing a pair dedups to the
  existing candidate.

### 2. Retrieval-affinity projection (new derived snapshot family)

- New family identifier `retrieval_affinity` added to the
  `graph_snapshots` CHECK constraint (real identifiers above; CLI aliases
  map via the same table `ee graph snapshot refresh` uses).
- **Accumulation**: an append-only consumption cursor over persisted pack
  ledger rows and search-result audit batches. For each unordered pair
  (a, b) co-occurring in one result set:
  `w(a,b) += 1 / (1 + |rank_a − rank_b|)`. At materialization, weights
  decay as `w · 2^(−Δt / half_life)` with
  `[graph.affinity] half_life_days = 30`. Re-running from the same cursor
  is idempotent (no double counting); snapshot content hash is
  deterministic for a given ledger prefix.
- **Privacy**: edges carry memory ids and counters only — no query text,
  no content.
- **THE HARD RULE**: this projection NEVER enters live search or pack
  ranking. Retrieval feeding ranking feeding retrieval would (a) break the
  byte-determinism contract and (b) self-reinforce popular memories into
  permanent dominance. Structural enforcement: the projection type is not
  registered in the retrieval feature-enrichment path, and a unit test
  asserts the search scoring config cannot reference it. Consumers:
  suggest-links and diagnostics only.
- Refresh: bounded steward job `retrieval-affinity-refresh` under the job
  ledger and lock discipline (same pattern as graph-snapshot-prune).

### 3. Conflict resolution verbs (`ee conflict resolve`)

`ee conflict resolve <a> <b> --verb <v> [--keep <id>] [--reason <text>]
[--apply]` — **dry-run is the default**; `--apply` is required to mutate
(the curate-disposition convention for all new write verbs in this wave).

| Verb | Durable effect (all via existing `apply_curation_candidate` machinery, one transaction scope, full audit) |
|---|---|
| `supersede` | requires `--keep`; loser gets supersede link + validity-window close via the standard lifecycle |
| `scope-split` | both survive; `--scope-a/--scope-b` tag/validity args annotate each side; link re-typed |
| `both-valid` | link re-typed to scoped-coexistence with rationale; contradiction cleared |
| `reject-one` | requires `--keep`; loser tombstoned via the existing curation path |

- Stale-surface guard: the live conflict surface is re-derived first; if
  the pair is no longer in conflict, refuse with
  `conflict_resolve_stale_surface` (low) and the current state.
- The rationale is persisted as a **decision-kind memory** linked to both
  sides, using the REAL registry fields: `options` (both memory ids +
  verbs considered), `chosen`, `rationale`, `supersedes` (loser id, when
  applicable). Future packs explaining the area carry the why.
- Policy: `reject-one` against a protected rule exits 7.

### 4. Graph diff (`ee graph diff`)

- Inputs: `--graph <family-or-alias>` (default `memory_links`), `--from` /
  `--to` snapshot ids, or `--since <RFC3339>` resolving to the nearest
  snapshot ≤ ts; defaults to the latest two. Missing snapshots →
  `graph_diff_snapshot_missing` (low; repair `ee graph snapshot refresh`).
- Output: node/edge add/remove sets (content-hash keyed, deterministically
  ordered — diff(A,B) sets are exact complements of diff(B,A));
  **community deltas** via stable fingerprint matching (Louvain labels are
  not stable across runs, so communities are matched by maximum member-set
  Jaccard overlap with a ≥0.5 same-community threshold; below threshold
  reports birth/death; moved memberships listed); **top-N centrality
  movers** ranked by |delta| from PERSISTED centrality rows at each
  snapshot version — absent rows are honestly omitted per side, never
  recomputed inline.
- Agent-sized output: summary counts first; bounded detail arrays with the
  ADR 0063 governor truncation point declared on each.

### 5. Degradation vocabulary

| Code | Severity | Class | Trigger |
|---|---|---|---|
| `suggest_links_insufficient_graph` | info | response_time | too few nodes/edges for any predictor to score |
| `retrieval_affinity_cold` | info | response_time | affinity snapshot absent; blend proceeds without that signal |
| `graph_diff_snapshot_missing` | low | response_time | requested from/to/since snapshot unavailable |
| `conflict_resolve_stale_surface` | low | response_time | conflict state moved since explain; re-run list |

Fixture/taxonomy files land with the emitting commits (bd-3a1op.2–.5) per
the same-commit rule. Schemas `ee.graph.suggest_links.v1`,
`ee.conflict.resolution.v1`, `ee.graph.diff.v1` ship standalone with those
commits; the field shapes in this ADR are normative.

## Consequences

- **Easier**: the graph densifies through reviewed suggestions (improving
  PPR/Pack-DNA/primer downstream); contradictions become a workflow with a
  durable decision trail; structural evolution becomes observable.
- **Guarded**: predictions and resolutions flow exclusively through
  audited curation machinery; the affinity feedback loop is structurally
  fenced out of live ranking; all outputs deterministic.
- **Costs accepted**: one new snapshot family + accumulation table +
  steward job; conflict resolution adds a decision memory per resolution
  (intentional — that record IS the value).

## Rejected Alternatives

- **Auto-applying high-confidence suggested links**: violates
  no-silent-mutation; rejected for candidates-only regardless of score.
- **Feeding retrieval-affinity into ranking** (even at tiny weight):
  determinism break + rich-get-richer rot; rejected with structural
  enforcement (§2).
- **Similarity-only suggestions** (dropping the `contradicts` type): thins
  a planned capability and hides exactly the disagreements reviewers must
  see; rejected (pass-2 scope clarification preserved).
- **Resolving cross-pair conflicts inside pack assembly by rank**: hides
  disagreement instead of resolving it; rejected — packs surface, resolve
  decides.
- **Recomputing centrality inside diff**: latency + duplicate machinery;
  rejected for persisted-rows-or-omit.
- **Fresh ad-hoc graph plumbing**: if the shared structural-graph adapter
  (bd-2pos6.7) lands first, suggest-links and diff MUST reuse it; recorded
  as a build-order obligation, not an alternative.

## Verification

- Unit (bd-3a1op.2/.3/.4/.5): predictor values against hand-computed
  micro-graphs; blend weights; candidate-bound enforcement (planted O(n²)
  trap); decay/cursor idempotency; affinity-isolation test (scoring config
  cannot reference the projection); verb mutation plans per fixture pair;
  stale-surface refusal; protected-memory exit 7; fingerprint matching on
  label-permuted fixtures (same structure ⇒ empty diff).
- Property (bd-3a1op.6): same seed/graph ⇒ identical suggestions;
  diff complement property.
- Differential (bd-3a1op.6, `differential-networkx` gate): AA/Jaccard
  cross-checked against Python NetworkX on sampled fixtures.
- E2E (bd-3a1op.6): `scripts/e2e_graph_intel.sh` — planted missing links
  found with explanations; --propose dedup; supersede resolution end-to-end
  (audit rows, decision memory, conflict list shrinks, superseded memory
  excluded from fresh pack); diff reports exactly the planted changes;
  `ee.test_event.v1` logging throughout.
