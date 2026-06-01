# ADR 0051: High-confidence co-tag auto-linking on `ee remember`

Status: Accepted
Date: 2026-06-01
Bead: bd-pp1fk

## Context

The graph layer is one of `ee`'s headline differentiators: PageRank, HITS,
PPR, Gomory-Hu proximity, dominance, causal paths, Pack DNA, and the knowledge
skyline all read from the memory-link graph. A 2026-05 lived-experience pass
found that this entire layer is **dormant by default**: 20 related `ee remember`
calls produced 0 links, so `ee insights` returned 14 empty sections and
`ee graph centrality` reported `graph_snapshot_missing`.

Root cause (see `src/core/memory.rs`):

- `create_auto_links_for_remember` only creates links between memories that
  share the **same `workflow_id`** (the Hebbian, workflow-recency signal). A
  plain `ee remember` with no `--workflow` produces zero links.
- `suggest_links_for_remember` computes **co-tag** neighbors (memories sharing
  tags), but those are emitted only as *advisory* `suggested_links` — never
  persisted. Turning a suggestion into a durable edge required an explicit
  `ee memory link` / curation step that agents almost never take.

The net effect: in normal use the graph never accumulates edges, so every
graph-derived surface degrades to empty. The `graph.no_links` insights signal
(shipped alongside the lived-experience fix) *tells* an agent the graph is
empty, but does nothing to populate it.

## Decision

On `ee remember`, additionally persist a **bounded, deterministic set of the
strongest co-tag neighbors** as durable `related` links, reusing the co-tag
candidates already computed for `suggested_links`.

Parameters (all in `src/core/memory.rs`):

- **Threshold**: `co_tag_score >= 0.75`. Because
  `co_tag_score = 0.55 + (matched/total)*0.4`, 0.75 means *at least half of the
  new memory's tags overlap the neighbor*. A single incidental shared tag never
  creates a durable link.
- **Fan-out cap**: at most 3 co-tag links per remember
  (`REMEMBER_AUTO_COTAG_LINK_LIMIT`).
- **Weight**: 0.4, just below the 0.5 workflow-recency weight, because co-tag
  is a weaker structural signal than same-workflow recency.
- **Determinism**: candidates arrive ordered by co-tag score then ULID payload,
  so the persisted prefix is byte-stable for a given DB + input.
- **Audit**: every link is written in a transaction with a
  `memory_link.create` audit entry (`ee.audit.memory_link_auto_create.v1`,
  `linkKind: "cotag"`). The links are automatic but **never silent** — see the
  principle reconciliation below.
- **Override**: gated on the existing `--auto-link` toggle (default on; disable
  with `--no-auto-link`). No new environment variable is introduced.
- **No double-counting**: persisted targets are removed from the advisory
  `suggested_links` set, so a memory is never both auto-linked and re-suggested.
  High-confidence co-tag pairs become edges; the lower-confidence remainder
  stays advisory.

Default posture is **on**, per the explicit product decision recorded on the
bead, because the headline gap otherwise persists for every user who never
flips a flag.

## Reconciliation with "No silent memory mutation"

`ee`'s product principle is *no silent* memory mutation, not *no automatic*
memory mutation (the workflow Hebbian links were already automatic). Every
co-tag link:

1. is written through the normal `insert_memory_link` path,
2. emits a `memory_link.create` audit entry visible in `ee memory history` and
   the audit log,
3. is reported in the `auto_links[]` array of the `ee remember` response with
   its `audit_id`,
4. carries provenance metadata (`matchedTags`, `cotagScore`) explaining *why*
   it was created,
5. is reversible via `ee memory link` management and bounded by the
   `--no-auto-link` opt-out.

This satisfies the auditability and explainability contracts: the mutation is
automatic but fully accounted for.

## Rejected alternatives

1. **Link-on-curate-only (status quo).** Keep co-tag pairs advisory and require
   an explicit `ee memory link`/curation apply. Rejected: this is exactly the
   behavior that left the graph dormant — agents do not run the explicit step,
   so the differentiator never materializes.

2. **Explain-only.** Ship only the `graph.no_links` signal plus the existing
   `suggested_links`, changing no write behavior. Rejected as insufficient on
   its own: it diagnoses the empty graph but never fills it. (It remains a
   useful complementary signal for the *untagged* case, where no co-tag signal
   exists.)

3. **Run a semantic search at remember-time and link top-K neighbors.** The
   bead originally framed this as "semantic-neighbor links". Rejected for v1:
   there is no embedding/search neighbor query in the remember path today, so
   this would add a non-trivial per-write cost (embed + search) and a new
   determinism surface. Co-tag overlap is already computed, deterministic, and
   free. A semantic variant can be layered on later behind its own flag if the
   tag signal proves insufficient.

4. **A new `EE_DISABLE_REMEMBER_*` environment variable.** Rejected: the
   existing `--auto-link` / `--no-auto-link` toggle already expresses "do/don't
   auto-link on remember". Adding a second control would fragment the surface
   and require a new `env_registry.rs` entry for no behavioral gain.

5. **Default-off, opt-in config flag.** Rejected per the product decision on the
   bead: a default-off capability leaves the headline gap in place for the
   common case, defeating the purpose.

## Verification hooks

- Unit: `persist_high_confidence_cotag_links` creates audited links only for
  candidates at/above threshold, respects the fan-out cap, and skips
  already-linked targets (`src/core/memory.rs` tests).
- Unit: `auto_link_status` reports `linked` whenever any durable link exists
  (workflow-recency or co-tag), including the workflow-less case.
- Behavioral: two tagged remembers sharing a majority of tags produce a durable
  `related` link with a `memory_link.create` audit entry, and the link is
  absent when `--no-auto-link` is set.

## Consequences

- The graph populates from ordinary tagged use, so `ee insights`,
  `ee graph centrality`, proximity, and Pack DNA become meaningful without an
  explicit linking ritual.
- `ee remember` responses for tagged memories with strong overlap now report
  co-tag entries in `auto_links[]` and correspondingly fewer `suggested_links`.
- Untagged remembers are unchanged: with no tags there is no co-tag signal, so
  the `graph.no_links` / `auto_link_disabled` advisory still guides the agent
  toward explicit linking or tagging.
