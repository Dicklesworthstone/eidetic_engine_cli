# ADR-0018: Plan Recommendation Ranking Algorithm

**Status:** Accepted  
**Date:** 2026-05-06  
**Bead:** eidetic_engine_cli-jfd9  
**Migration:** V035_PLAN_RECIPES (src/db/mod.rs:3110)

## Context

The `ee plan recommend <task>` command needs to recommend recipes from two sources:
1. Static builtin recipes (already implemented in `src/core/plan.rs`)
2. User-defined plan recipes stored in the `plan_recipes` table

The recommendation must be:
- **Deterministic**: Same input, same database state → same ranking
- **Explainable**: Each recommendation includes score components
- **Evidence-backed**: Recipes with more supporting evidence rank higher

## Decision

### Ranking Algorithm

Use a weighted hybrid scorer consistent with `ee context` and `ee search`:

```
score = w_text * text_similarity
      + w_semantic * semantic_similarity
      + w_maturity * maturity_score
      + w_recency * recency_decay
      + w_evidence * evidence_count_normalized
```

**Weights (default):**
| Component | Weight | Rationale |
|-----------|--------|-----------|
| text_similarity | 0.30 | BM25/FTS5 match against task description |
| semantic_similarity | 0.25 | Vector similarity via Frankensearch |
| maturity_score | 0.20 | draft=0.3, validated=0.6, promoted=1.0 |
| recency_decay | 0.10 | Prefer recently updated recipes |
| evidence_count | 0.15 | Recipes with more evidence_uris rank higher |

### Tie-Breaking

Implementation details: Frankensearch BM25 scores are normalized as `s/(1+s)`;
semantic similarity uses Frankensearch cosine similarity clamped to `[0,1]`.
At least one text hit or semantic similarity of 0.5 is required before metadata
can contribute. Static catalog entries have no learned maturity or recorded
recency and receive zero for those components. Evidence is deduplicated after
redaction and normalized as `n/(1+n)`; redacted links receive no evidence credit.
Built-in recipes retain their catalog URI for explanation, but that
self-reference earns no supporting-evidence credit.
Recency uses a 30-day half-life relative to the newest stored `updated_at`,
reported as `recencyAnchor`, so unchanged reads do not drift with wall time.
The score is a ranking value, not a calibrated confidence probability.

When scores are equal within epsilon (1e-6):
1. Sort by maturity descending (promoted > validated > draft)
2. Sort by created_at ascending (older recipes first for stability)
3. Sort by id lexicographically

To keep comparison transitive, first sort exact scores, then form consecutive
epsilon groups relative to each group's highest score and apply these keys.
`matchesFound` counts qualifying matches before the output limit.

### Degraded Mode

If semantic search is unavailable:
- Set `semantic_similarity = 0` for all candidates
- Redistribute weight to `text_similarity` (0.30 → 0.55)
- Include `degraded: ["semantic_search_unavailable"]` in response

The wire form uses a structured degraded entry with code, warning severity,
and message. When lexical retrieval is excluded from the build but a semantic
model is usable, its weight moves to semantic similarity (0.25 → 0.55).
If neither retrieval arm is usable, return a search error.

### Score Components in Response

Each recommendation includes:
```json
{
  "recipe_id": "...",
  "score": 0.85,
  "components": {
    "text_similarity": 0.90,
    "semantic_similarity": 0.75,
    "maturity_score": 1.0,
    "recency_decay": 0.95,
    "evidence_count": 0.80
  },
  "rank": 1,
  "explanation": "High text match for 'release workflow', promoted maturity, 3 evidence links"
}
```

## Consequences

### Positive
- Consistent with existing retrieval scoring in `ee context`
- Fully explainable recommendations
- Graceful degradation when semantic search unavailable
- Deterministic tie-breaking for stable golden tests

### Negative
- Requires plan_recipes table migration (V035)
- Additional complexity in CLI output formatting

### Verification
- Golden tests for empty store, single match, multi-match-with-tie, no-match
- Determinism test: run recommend twice, compare rankings byte-for-byte

## Alternatives Considered

1. **Keyword-only matching**: Rejected — current `classify_goal` is too coarse
2. **LLM-based ranking**: Rejected — violates local-first, non-deterministic
3. **Graph-based PageRank**: Deferred — useful but V1 should use simpler scorer
