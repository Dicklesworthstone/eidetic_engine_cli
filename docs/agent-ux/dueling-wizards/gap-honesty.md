# Dueling-Wizards Gap-Honesty: Blind-Spot Map + Query-Miss Clustering

Agent-facing companion for the gap-honesty surfaces introduced under
`bd-1n0np.6`. These surfaces give an agent a *calibrated caution signal*: a
bounded, honest answer to "what does the memory store NOT cover for this
codebase, and which repeated searches keep missing?" — before the agent relies
on memory for an area the store cannot speak to.

The design invariant: gap-honesty surfaces are **read-only and advisory**. They
never mutate memory, never auto-promote a miss into a stored fact, and never
hide a truncation. A gap is a *prompt for review*, not a conclusion.

## Existing Anchors

| Anchor | Role |
| --- | --- |
| `src/cli/insights/mod.rs` | `ee insights --section blindSpots` / `--section knowledgeGaps` section builders + dispatch. |
| `docs/schemas/ee.insights.v1.json` | JSON contract for the insights bundle, incl. the blind-spots and knowledge-gaps sections. |
| `src/core/query_miss_cluster.rs` | Pure, deterministic query-miss clustering (`cluster_query_misses`, `query_cluster_key`, `KnowledgeGapCandidate`). |
| `scripts/e2e_gap_honesty.sh` | Real-binary, capability-guarded e2e for the whole gap-honesty path (`bd-1n0np.6.6`). |
| `docs/agent-ux/dueling-wizards-determinism-gate.md` | Determinism requirements the blind-spots / knowledge-gaps JSON must satisfy. |

## Blind-Spot Map (`ee insights --section blindSpots`)

The blind-spot map is **set arithmetic over a workspace snapshot**, independent
of the anchor table (anchors enrich it; they are not required):

```
blind spots = symbol-graph nodes
            − nodes referenced by memory file:// provenance
            − nodes referenced by a lexical path/symbol mention in a memory
```

Schema: `ee.insights.blind_spots.v1` (see `docs/schemas/ee.insights.v1.json`).
Command:

```bash
ee insights --section blindSpots --workspace . --json
```

Properties an agent can rely on:

- **Works without anchors.** The code side is the symbol graph, which exists
  independently of any memory. Coverage is computed from provenance + lexical
  mentions that are present today; the anchor table only sharpens it.
- **Deterministic over a snapshot.** Given the same DB, indexes, and workspace
  tree, the section is byte-identical. Churn is supplied as a *bounded git
  input*, never `Date::now()` — so the output does not drift with wall-clock.
- **Honest coverage, not noise.** The section carries a `coverageRatio` rather
  than dumping "everything is uncovered." Items are ranked by an explicit
  `importanceScore` and `rankingBasis` so agents can see which signals were
  available.

### Reading `coverageRatio`

`coverageRatio` is a number in `[0, 1]`: the fraction of symbol-graph nodes that
at least one memory references (by provenance or lexical mention).

| `coverageRatio` | Interpretation | Suggested posture |
| --- | --- | --- |
| near `1.0` | Most code is referenced by some memory. | Trust memory for most areas; check the listed blind spots before the rest. |
| mid-range | Substantial uncovered surface. | Treat memory as partial; corroborate against source for blind-spot nodes. |
| near `0.0` | Almost nothing is covered. | Do not rely on memory for code areas; the store is effectively cold here. |

A low ratio is not a failure — it is the honest report. Use the listed
blind-spot nodes as the priority list of where to read source directly.

### Reading `importanceScore`

`importanceScore` is currently:

```
locLines * gitChurnFactor * centralityFactor
```

The `rankingBasis` object carries the exact inputs. `gitChurnLines` is read
from a bounded `git log --max-count=200 --numstat` scan, so it is deterministic
for the checked-out repository history and does not depend on wall-clock time.
When git evidence is unavailable, `gitChurnStatus` is `unavailable` and the
factor is neutral (`1.0`) instead of silently zeroing a code area.

`centralityStatus` is explicit because the current symbol snapshot has nodes
but no dependency edges to feed fnx centrality. Until symbol-edge centrality
lands, `centralityScore` is `null` and `centralityFactor` is neutral (`1.0`).
That keeps the ranking useful while making the missing signal machine-visible.

> **Remaining `bd-1n0np.6.2` work.** The top-N cut with its drop-count and the
> per-pack `coverage: thin` marker are still pending. `scripts/e2e_gap_honesty.sh`
> records visible `log_drop` entries for those assertions so the gap is never
> silently presented as covered.

## Query-Miss Clustering → Knowledge-Gap Candidates

Repeated low-utility searches (queries that keep returning nothing useful) are
recorded in the query-miss ledger and clustered into advisory knowledge-gap
candidates.

Clustering is pure and deterministic (`src/core/query_miss_cluster.rs`):

- `query_cluster_key(query)` derives an **order-independent, normalized
  token-set key**, so paraphrases of the same need collapse into one cluster
  ("kubernetes pod eviction policy" and "pod eviction policy for kubernetes"
  land together).
- `cluster_query_misses(...)` groups misses by that key and emits a
  `KnowledgeGapCandidate` only once a cluster reaches
  `KNOWLEDGE_GAP_MIN_CLUSTER_MISSES` (currently `3`) — a single stray miss never
  becomes a candidate.
- Blank queries are ignored; grouping is deterministic for a fixed miss set.

A `KnowledgeGapCandidate` is **advisory only**. It surfaces in
`ee swarm brief` (and via the `knowledgeGaps` insights section) as a prompt for
a steward/human to decide whether the gap warrants new memory. Nothing about a
miss is auto-promoted into a stored fact: strict review precedes any write.

### Miss-ledger TTL + redaction (contract)

Misses are operational telemetry, not durable knowledge. The intended contract:

- **TTL.** Ledger rows expire on a bounded TTL so the ledger reflects *current*
  search behaviour, not all-time history; expired rows drop out of clustering.
- **Redaction.** A raw query may carry sensitive content, so stored miss rows
  are redaction-eligible — clustering keys (normalized token sets) are retained
  for grouping while the verbatim query is subject to redaction policy.

> **Config registration (`bd-1n0np.6.7`, in progress).** The concrete
> `[query_miss]` TTL/redaction config keys are not yet registered in the config
> surface. This section documents the contract those keys must honour; the
> implementation slice that registers them should update this table with the
> exact key names and defaults rather than inventing them here.

## Conservatism Summary

| Surface | Conservative rule |
| --- | --- |
| Blind-spot map | Reports membership + an honest `coverageRatio`; never claims full coverage; any top-N cut logs its drop-count. |
| Knowledge-gap candidate | Advisory; requires `≥ KNOWLEDGE_GAP_MIN_CLUSTER_MISSES` paraphrased misses; never auto-becomes memory. |
| Miss ledger | TTL-bounded + redaction-eligible; verbatim queries are not retained beyond policy. |

Every truncation, sampling, or abstention these surfaces perform is reported
(the no-silent-cap rule): a missing surface in `scripts/e2e_gap_honesty.sh`
records a `log_drop` carrying the exact assertion that activates once the
surface lands, never a false pass.
