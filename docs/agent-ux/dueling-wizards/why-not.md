# `ee why-not` — counterfactual exclusion explainer

`ee why-not` is the read-only reverse of [`ee why`](insights-onboarding.md). Where
`ee why <id>` explains why a memory **was** stored, retrieved, and selected,
`ee why-not <id> --task "<task>"` explains why a memory **was not** selected for
the context pack a given task would build — and the minimal change that would
flip exclusion to inclusion.

It costs one `ee pack`-shaped retrieval. It never persists a pack record, never
mutates the store, and never reaches into the agent loop. Use it when a memory
you expected to show up in `ee pack "<task>"` is missing and you need a precise,
machine-readable reason rather than a guess.

Bead lineage: `bd-1n0np.1` (feature), `bd-1n0np.1.2` (CLI surface + core handler),
`bd-1n0np.1.4` (authoritative vs reconstructed honesty contract).

## Command

```bash
ee why-not <memory-id> --task "<task>" --workspace . --json
```

| Flag | Purpose |
|---|---|
| `<memory-id>` | The memory whose exclusion you want explained (positional). |
| `--task "<task>"` | The task/query the hypothetical context pack is built for (required). |
| `--profile <p>` | Retrieval profile (`compact`, `balanced`, `grounding`, `orientation`, `thorough`, `submodular`). |
| `--max-tokens <N>` | Token budget for the hypothetical pack (defaults to the pack default). |
| `--candidate-pool <N>` | Candidate pool size for retrieval. |
| `--database <path>` | Database path; defaults to `<workspace>/.ee/ee.db`. |

Output renders as JSON (`--json`), markdown (default `--format markdown`),
human, or TOON, mirroring `ee why` so agents can use both interchangeably.

## JSON contract

The envelope is `ee.response.v2`; `data` is the `ee.why_not_selected.v1` report:

```jsonc
{
  "schema": "ee.response.v2",
  "success": true,
  "data": {
    "schema": "ee.why_not_selected.v1",
    "memoryId": "mem_...",
    "selected": false,
    "retrievalStageReached": "retrieval",
    "primaryReason": "not_retrieved",
    "reasonSource": "reconstructed",
    "scores": {
      "targetRelevance": 0.0,
      "targetUtility": 0.74,
      "targetComposite": 0.0,
      "lastIncludedMemoryId": "mem_...",
      "lastIncludedComposite": 0.61
    },
    "scoreDeltaToLastIncluded": -0.61,
    "tokenBudgetFrontier": {
      "maxTokens": 4000,
      "usedTokens": 3960,
      "targetEstimatedTokens": 120,
      "requiredAdditionalTokens": 80
    },
    "counterfactualHints": [
      { "kind": "budget", "action": "raise --max-tokens", "rationale": "..." }
    ],
    "filtersApplied": [],
    "redactionScopeExclusions": [],
    "degraded": [],
    "provenance": []
  }
}
```

The report deliberately omits the target memory's body — it is an explanation,
not a content surface. Use `ee memory show <id>` to read the body.

## The honesty contract: `reasonSource`

`reasonSource` is the load-bearing field for trusting a why-not answer:

| `reasonSource` | When | What it means |
|---|---|---|
| `authoritative` | The target memory **was** in the retrieved candidate pool and was then excluded by a known selector stage (token budget, redundancy, scope, redaction, validity window, policy, or filter). | The reason came straight from the selector's own decision. Trust it. |
| `reconstructed` | The target memory **never reached** the candidate pool (`primaryReason` is `not_retrieved` or `not_retrieved_due_to_degraded_index`). | `ee why-not` re-ran only the retrieval arm to infer the exclusion. It is advisory: the memory simply did not surface for this query, possibly because of weak query recall or a degraded index. |

Never present a `reconstructed` answer as authoritative. If you need a precise,
in-selector reason, refine `--task` so the memory enters the candidate pool, then
re-run — `reasonSource` will flip to `authoritative` once the selector itself
makes the exclusion decision.

## The three canonical fixes

`counterfactualHints[]` carries the minimal change that would flip exclusion to
inclusion. The three canonical levers are:

1. **More budget** (`kind: "budget"`) — the target fit on score but lost at the
   token wall. Raise `--max-tokens` (see `tokenBudgetFrontier.requiredAdditionalTokens`)
   or switch to `--profile compact` to free room.
2. **Fewer competitors** (`kind: "redundancy"` / selection pressure) — the target
   was crowded out by higher-scoring or near-duplicate candidates. Narrow `--task`,
   raise its specificity, or prune redundant memories.
3. **Higher confidence/relevance** (`kind: "score"`) — the target scored below the
   inclusion frontier. See `scoreDeltaToLastIncluded` for the gap; improve the
   memory's evidence/utility (`ee outcome <id> --signal helpful`) or its query
   match so its composite clears the last-included score.

## The "actually selected" path

If the target memory **was** selected for the task, `ee why-not` does not error —
it reports `selected: true`, `primaryReason: "selected"`, and the item's rank and
section, keeping the envelope shape consistent with the excluded case. This makes
`ee why` and `ee why-not` safe to call interchangeably from an agent harness.

## Worked example

```bash
ee why-not mem_release_policy --task "prepare release" --workspace . --json \
  | jq '.data | {selected, primaryReason, reasonSource, scoreDeltaToLastIncluded, hints: .counterfactualHints}'
```

```json
{
  "selected": false,
  "primaryReason": "omitted_by_token_budget",
  "reasonSource": "authoritative",
  "scoreDeltaToLastIncluded": 0.04,
  "hints": [
    { "kind": "budget", "action": "raise --max-tokens by ~80", "rationale": "Target fit on score but lost at the token wall." }
  ]
}
```

Here the memory was authoritatively excluded by the token budget despite scoring
**above** the last included item by `0.04` — raising `--max-tokens` by the
`requiredAdditionalTokens` amount includes it.

## See also

- [`insights-onboarding.md`](insights-onboarding.md) — `ee why` and graph-derived explanation surfaces.
- [`adaptive-pack-budget.md`](adaptive-pack-budget.md) — how the token budget that `ee why-not` reasons against is computed.
- `docs/schemas/ee.why_not_selected.v1.json` — the canonical report schema.
