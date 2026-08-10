# Graph intelligence: densification, conflict resolution, temporal diff

ADR 0066 (bd-3a1op). Three shipped surfaces close the loop from "the graph is
missing structure" to "the graph changed and here is exactly how":

1. `ee graph suggest-links` — typed link prediction over bounded candidates.
2. `ee conflict resolve` — audited resolution of one conflicting pair.
3. `ee graph diff` — temporal structural diff between persisted snapshots.

All three are agent-first: JSON reports carry raw evidence (per-signal values,
per-atom audit ids, per-array truncation flags), and every refusal names a
stable code with a repair command.

## The densification loop

Sparse memory graphs starve pack assembly, `ee why`, and structural health.
The loop that fixes that without ever letting a model write links silently:

```bash
# 1. Predict: bounded candidates, blended fnx-backed signals, typed relations.
ee graph suggest-links --workspace . --json

# 2. Propose: write curation candidates (contradicts-typed suggestions become
#    contradiction_review candidates). NEVER creates links directly; re-running
#    dedups to the existing candidate ids.
ee graph suggest-links --workspace . --propose --json

# 3. Review + apply through the standard curation lifecycle:
ee curate validate <candidate-id> --workspace . --json
ee curate apply    <candidate-id> --workspace . --json   # creates the typed link

# 4. Observe the structural change:
ee graph snapshot refresh --graph memory_links --workspace . --json
ee graph diff --graph memory_links --workspace . --json
```

Signals per suggestion row (raw values always carried): `adamicAdar`,
`preferentialAttachment` (fnx over the shared-neighbor graph), `jaccardTags`,
`ppr` (symmetrized), `affinity` (retrieval co-selection; honestly omitted
while the [retrieval-affinity snapshot](../architecture/graph-snapshots.md) is
cold — the report sets `affinityCold` and emits `retrieval_affinity_cold`).
Blend weights are ADR constants in `src/core/suggest_links.rs` (0.35 AA /
0.20 PA / 0.25 Jaccard / 0.15 PPR / 0.05 affinity, per-batch min-max
normalized); there are no `[graph.suggest]` config keys yet — treat the
constants as the contract until a config surface ships.

Typing: token-Jaccard ≥ 0.5 with opposed polarity (negation on exactly one
side) → `contradicts`; same-polarity overlap → `supports`; otherwise
`related`. An empty or too-sparse graph reports
`suggest_links_insufficient_graph` instead of relaxing the candidate bound.

## Conflict resolution recipe

`ee conflict list|explain|cluster` is the read-only surface
(`ee.conflict.v1`): ranked conflicting pairs with both bodies, the preferred
side (`higher_trust` / `fresher` / `tie_no_signal`), and contradiction
clusters. `ee conflict resolve` acts on ONE pair:

```bash
ee conflict list --workspace . --json          # pick a pair + a verb
ee conflict resolve <mem-a> <mem-b> --verb supersede --keep <mem-a> \
    --reason "A reflects the current release gate; B predates it" \
    --workspace . --json                       # DRY-RUN: prints the plan
# same command + --apply executes it
```

Verb → audited atoms (`ee.conflict.resolve.v1`; no novel mutation paths —
each planned action is an existing audited core operation):

| Verb | Mutations | Notes |
|---|---|---|
| `supersede --keep K` | one `decide record` atom: decision memory + `supersedes` link + loser validity close | the single-atom path; all three audit ids reported |
| `reject-one --keep K` | `memory expire` on the loser + decision memory | policy-denied (exit 7) when the loser is a human-explicit rule |
| `scope-split --scope-a-tags .. --scope-b-tags ..` | audited tag patches on both sides + decision memory | both scopes required |
| `both-valid` | `related` link carrying `resolution=both_valid` metadata + decision memory | records that the tension is legitimate |

Contracts to rely on:

- **Live-surface gate.** The pair is re-derived at resolve time; if state
  moved since you ran `explain`, the command refuses with
  `conflict_resolve_stale_surface` and returns the focused current pairs.
- **Terminal.** A tombstoned side drops the pair from the actionable surface,
  so a successful `supersede`/`reject-one` cannot be re-applied.
- **The WHY persists.** The rationale lands as a `kind=decision` memory
  (typed fields `topic`/`chosen`/`alternatives`/`rationale`/`supersedes`), so
  future packs explaining the area carry the decision.
- **Known gap:** `both-valid` does not yet suppress the pair from
  `conflict list` (the detector does not read resolution metadata); the
  decision memory and link are still recorded.

## Temporal graph diff

`ee graph diff` compares two PERSISTED snapshots of one family — it never
recomputes centrality inline and never invents a missing side:

```bash
ee graph diff --graph memory_links --workspace . --json         # latest two
ee graph diff --from <snapshot-id> --to <snapshot-id> --json    # explicit
ee graph diff --since 2026-08-01T00:00:00Z --json               # nearest ≤ ts as from
```

Report shape (`ee.graph.diff.v1`), summary counts first:

- **Add/remove sets** for nodes and edges, keyed by content hash (blake3 over
  the canonical `src|relation|dst|directed` string, endpoint-canonical for
  undirected edges). `diff(A,B)` sets are exact complements of `diff(B,A)`.
- **Community deltas.** Louvain labels are not stable across runs, so
  communities are matched by maximum member-set Jaccard: ≥ 0.5 is the same
  community (membership churn listed; zero-churn matches only counted),
  below threshold reports births/deaths.
- **Centrality movers.** Top 10 by |Δ pagerank| strictly from the values
  persisted inside each snapshot; nodes missing a persisted value on either
  side are omitted and counted in `summary.centralityOmitted`.
- **Bounded detail.** Every detail array truncates at the declared
  `detailCap` (64) with a per-array `truncated` flag — the governor point is
  in the report, never silent.

Fewer than two usable snapshots emits `graph_diff_snapshot_missing` (low)
with repair `ee graph snapshot refresh` instead of failing or fabricating.

## Degradation vocabulary

| Code | Severity | Surface |
|---|---|---|
| `suggest_links_insufficient_graph` | info | `graph suggest-links` |
| `retrieval_affinity_cold` | info | `graph suggest-links` |
| `conflict_resolve_stale_surface` | medium | `conflict resolve` |
| `graph_diff_snapshot_missing` | low | `graph diff` |

E2E coverage: `scripts/e2e_graph_intel.sh` (verify.sh stage 6.1269) walks the
full arc — prediction, propose+dedup, curate apply, conflict surface, resolve
dry-run/apply/stale-refusal, and a planted-growth snapshot diff.
