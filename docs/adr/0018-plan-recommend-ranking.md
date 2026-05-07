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

When scores are equal within epsilon (1e-6):
1. Sort by maturity descending (promoted > validated > draft)
2. Sort by created_at ascending (older recipes first for stability)
3. Sort by id lexicographically

### Degraded Mode

If semantic search is unavailable:
- Set `semantic_similarity = 0` for all candidates
- Redistribute weight to `text_similarity` (0.30 → 0.55)
- Include `degraded: ["semantic_search_unavailable"]` in response

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
