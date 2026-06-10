# ADR 0070: Outcome-Tuned Retrieval Weights via Shadow Policy

Status: proposed
Date: 2026-06-10
Bead: bd-2tehh.1 (epic bd-2tehh, 2026-06 idea-wizard wave)

## Context

Retrieval mixing weights are global constants (`SearchScoringConfig` in
`src/search/scoring.rs`: `graph_centrality_weight` 0.10,
`recency_tau_days` 30, bias caps — fact-checked 2026-06-10): a docs-heavy
workspace and a code-archaeology workspace rank identically even when
outcome signals show systematic misses. This ADR specifies an OFFLINE
weight-tuning loop on the existing shadow-policy infrastructure
(`src/shadow.rs`): a new shadowable policy
`candidate.retrieval.outcome_tuned_weights` that replays historical queries
against candidate weight vectors, scores them with outcome-labeled
relevance, and changes live ranking ONLY through an explicit promote step
that writes a pinned, hash-stamped config overlay — after which ranking is
again 100% deterministic. Fact-check correction folded in:
`promote_shadow_policy_from_score()` already exists; the CLI/overlay work
extends that path rather than building a parallel verb. The nickname is
"bandit" but the design deliberately rejects stochastic bandits: at
workspace scale (hundreds of labeled triples) reproducibility outranks
regret bounds.

## Decision

### 1. Label extraction

Labeled triples `(query, memory, signal, weight, age)` are joined from
persisted state only:

- **Dense source**: pack-item outcomes (`ee outcome --pack <id> --item
  <n>`, bd-1pi9m.5) — the pack ledger maps item → memory, the pack task is
  the query. Label weight 1.0.
- **Weak source**: direct memory outcomes recorded within a window
  (default 30 min, `[shadow.retrieval] label_window_minutes`) of a
  `search.returned_mem` audit row for that memory — query taken from the
  audit batch. Label weight 0.5 (temporal association, not proof of use).
- Quarantined feedback events are EXCLUDED by construction (the
  trust-model integration: poisoned feedback never reaches tuning).
- Freshness discount: label weight × `2^(−age_days/90)`.
- Every triple carries provenance (event ids); the evaluation report hashes
  the label set for reproducibility.

### 2. Evaluation metric

For each candidate weight vector, historical queries are re-executed
against the CURRENT index with an injected `SearchScoringConfig` override
(an eval-only injection point unreachable from live CLI paths — enforced by
a unit test asserting no CLI arg reaches it). Metric: outcome-weighted
rank quality —

```text
score(vector) = Σ_q  norm_q · Σ_(m,s,w)  w · gain(s) / log2(1 + rank_q(m))
  gain: helpful/confirmation = +1, harmful/contradiction = −2
        (harmful dominance mirrors the curation harmful_weight asymmetry)
  norm_q: per-query normalization (1 / Σ|w·gain|) so chatty workspaces
          cannot dominate; unranked labeled memories contribute 0
  ties:   deterministic (existing search tie-breaks apply)
```

### 3. Search space (deterministic, guard-railed)

- Enumerated, no RNG: incumbent vector + a fixed offset grid (±0.05, ±0.10
  on each of lexical/semantic/graph weights and recency tau ±10d) +
  bounded coordinate descent around the grid winner (≤ 2 rounds, step
  halving). Total ≤ ~40 vectors, evaluated in deterministic order with
  `&Cx` cancellation checkpoints between vectors.
- **Clamps** (a degenerate label set must not produce a pathological
  overlay): lexical and semantic weights ∈ [0.2, 0.7]; graph weight
  ∈ [0.0, 0.3]; recency tau ∈ [7, 120] days. Clamps live in code next to
  the policy, not in user config.

### 4. Evidence gate (abstention contract)

Below **50 labeled triples** or **15 distinct queries** (`[shadow.retrieval]
min_triples / min_queries`), the policy ABSTAINS per the shadow-inventory
abstention rules with `insufficient_outcome_evidence` (info). Tune the
thresholds with data; never remove them. Promotion additionally requires
the winner to beat the incumbent by ≥ 3% relative metric margin
(`[shadow.retrieval] promote_margin`), and the report must be fresh
(evaluated at the current DB generation).

### 5. Promotion mechanics (audit-and-extend the existing path)

- Inventory: new domain `retrieval_weights`, policy
  `candidate.retrieval.outcome_tuned_weights`, maturity `Experimental`
  (PolicyMaturity ladder applies for graduation).
- `ee shadow run --policy candidate.retrieval.outcome_tuned_weights`
  executes the evaluator and persists an
  `ee.shadow.retrieval_tuning_report.v1` (incumbent score, per-candidate
  scores, winner, evidence counts, label-set hash, abstention reason when
  applicable). Score/verdict reuse the existing
  `ee.shadow_policy_score/verdict.v1` contracts.
- **Promote**: bd-2tehh.3 first AUDITS the existing
  `promote_shadow_policy_from_score()` semantics (what it mutates, what it
  audits, what reaches it) and EXTENDS it — no parallel path — to: validate
  the verdict (margin, evidence gate, generation freshness), write the
  `[search]` overlay keys into `<workspace>/.ee/config.toml` via
  `toml_edit` (format-preserving), record an audit row with policy id,
  report hash, overlay hash, and PRIOR VALUES (RULE 1: nothing lost), and
  print the exact diff. Refusal is exit 7 with structured reasons.
- **Demote** restores the prior values from the audit trail; config
  restoration is byte-identical (tested).
- Post-promotion honesty: subsequent search/pack responses carry
  `retrieval_weights_overlay_active` (info, with the overlay hash) so
  ranking changes are attributable. **Pack hashes legitimately change at
  promotion** — config is a pack-hash input and THAT IS CORRECT; eval
  fixtures pin configs (asserted in the e2e).

### 6. Determinism story (the part reviewers probe)

Adaptation happens offline. Live ranking changes only when promote writes
the pinned overlay; after promotion, same DB + same config ⇒ same bytes,
exactly as before. The overlay is ordinary config — no runtime learning, no
per-query dynamics, no hidden state.

### 7. Degradation vocabulary

| Code | Severity | Class | Trigger |
|---|---|---|---|
| `insufficient_outcome_evidence` | info | response_time | evidence gate not met; evaluator abstained |
| `retrieval_weights_overlay_active` | info | response_time | a promoted overlay is in effect (carries overlay hash) |

Fixture/taxonomy files land with the emitting commits (bd-2tehh.2/.3).

## Consequences

- **Easier**: each workspace earns its own retrieval mix from real
  outcomes, with a reviewable report and an explicit, reversible apply.
- **Guarded**: clamps, evidence gate, quarantine exclusion, margin
  requirement, audit-with-prior-values, attributable overlay hash;
  determinism contract intact end-to-end.
- **Costs accepted**: label density gates usefulness — this epic
  deliberately sits LAST in the wave's value chain (bd-2tehh.2 blocks on
  pack-item outcomes); replay cost bounded by the ≤40-vector budget and
  cancellation.

## Rejected Alternatives

- **Online/stochastic bandits** (Thompson, UCB): break reproducibility and
  auditability for regret bounds irrelevant at this scale. Rejected — point
  future proposals here.
- **Per-query dynamic weights**: byte-determinism break. Rejected.
- **Editing score-calibration files**: calibration interprets scores AFTER
  weighted fusion; tuning edits the fusion weights BEFORE it. The
  interaction order is fixed (calibration applies post-fusion) and tuning
  never touches calibration state.
- **A parallel promote verb**: superseded by the fact-check —
  `promote_shadow_policy_from_score()` exists; audit-and-extend.
- **Auto-promotion on a winning report**: violates the explicit-apply
  principle for ranking-affecting change. Rejected.

## Verification

- Unit (bd-2tehh.2): label-join correctness on fixture DBs (quarantine
  exclusion, window edges, weight/discount math); metric hand-computed on
  micro-cases; enumeration determinism (two runs ⇒ byte-identical
  reports); evidence-gate abstention; clamp enforcement; cancellation
  mid-sweep leaves no partial state; injection-point isolation.
- Unit (bd-2tehh.3): existing-promote-path audit findings recorded;
  promote validation matrix (stale report, abstained report, margin miss,
  generation drift); toml_edit format preservation; demote byte-identity;
  audit completeness incl. prior values.
- Property (bd-2tehh.4): evaluator monotonicity — adding a helpful label
  for a memory never lowers the score of vectors ranking it higher.
- E2E (bd-2tehh.4): `scripts/e2e_shadow_retrieval_tuning.sh` — planted
  semantic-bias corpus via real CLI outcomes; run → winner; sparse fixture
  → exit-7 abstention on promote; promote on the planted fixture →
  overlay diff + audit + `retrieval_weights_overlay_active` + ranking
  actually changed + pack hash changed attributably; demote → byte-
  identical config + ranking reverts. `ee.test_event.v1` logging
  throughout.

## Appendix: `ee.shadow.retrieval_tuning_report.v1` (normative draft)

```text
object ee.shadow.retrieval_tuning_report.v1
  schema          const "ee.shadow.retrieval_tuning_report.v1"
  policyId        const "candidate.retrieval.outcome_tuned_weights"
  dbGeneration    integer
  labelSet        {triples: integer, distinctQueries: integer,
                   hash: string, denseShare: number}
  abstained       boolean
  abstentionReason string | null
  incumbent       {weights: object, score: number}
  candidates[]    {weights: object, score: number}   (deterministic order)
  winner          {weights: object, score: number,
                   relativeMargin: number} | null
  promotable      boolean
  reportHash      string
```
