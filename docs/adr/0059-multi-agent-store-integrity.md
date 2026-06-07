# ADR 0059: Multi-Agent Store Integrity (Context Read Fences + Memory Write Immune System)

Status: proposed
Date: 2026-06-07
Bead: bd-1n0np.8.1

## Context

`ee` is explicitly used in crowded checkouts and swarms (AGENTS.md: peer WIP
changes "multiple times per minute"; the README sells a "crowded-agent posture").
Yet every existing safety mechanism guards *shell commands* (trauma-guard) or
*single-memory content* (injection guard). Two integrity gaps remain — both
surfaced independently as the post-duel "blind spots" of the 2026-06-07 review,
each targeting `ee`'s actual deployment model:

1. **Reads**: a pack can be assembled from **mixed generations** (search index at
   gen 120, DB rows after gen 124, graph snapshot at gen 118, a pack-cache entry
   under old policy) and nobody can tell whether it was coherent at a single
   logical point in time.
2. **Writes**: nothing protects the shared store from a malfunctioning, looping,
   or adversarial *writer* flooding everyone's packs with low-quality or
   subtly-wrong memories.

## Decision

Two complementary halves under one epic.

**A. Coherent Context Read Fences.** A monotonic workspace generation increments
on durable memory/curation/link/tombstone writes; search index metadata and
graph snapshots expose `source_generation`; the pack L2 cache key includes the
fence generation + policy/lens hash. A `ReadFence` threads through
`core::search/context/why`, and every machine-readable response gains an additive,
stable-ordered `consistency` block (`ee.context_read_fence.v1`: mode, coherent,
per-asset generations, staleAssets, repair). Modes: `eventual` (default;
**reports** gaps honestly), `latest` (require assets ≥ DB generation, else
high-severity degraded / fail in strict), `snapshot:<id>` (replay a pinned
generation). New degraded codes: `index_behind_read_fence`,
`graph_behind_read_fence`, `cache_generation_mismatch`, `snapshot_unavailable`.

**B. Memory Write Immune System.** At the single write-owner chokepoint
(ADR 0013) — already serialized — compute deterministic per-source rolling stats
(writes/window, near-duplicate ratio via `content_hash` + embedding similarity,
trust-class distribution, evidence-presence ratio) over **explicit windows**.
Source = `EE_AGENT_NAME` / import batch / mesh peer. A threshold trip sets the new
memory `trust_class = quarantined` (already held back from packs), writes an audit
row, and routes the batch to `curate`; `ee curate quarantine` offers accept/reject.
This is the **write-side analogue** of the existing harmful-feedback quarantine
(`[feedback] harmful_per_source_per_hour`). It is a **per-source advisory hold,
never a global write stall**, with an orchestrator whitelist for prolific legit
agents. v1 needs no outcome data; a v2 folds in outcome-weighted reputation once
the harvester (ADR 0055) exists.

## Consequences

- **Easier**: any context-producing command can state "coherent as of generation
  N" or "used an index 4 writes behind the DB; here is the repair"; the shared
  store gains an immune response to anomalous write streams.
- **Guarded**: the default (`eventual`) path stays fast (consistency block
  overhead negligible); quarantine is per-source + review-not-remove + whitelisted
  to avoid false holds.
- **Intentionally impossible**: no global write stall; no strict-fence slowdown of
  the default path; stats/generations never use wall-clock.

## Rejected Alternatives

- **Make `latest` the default**: would slow every read; rejected (`eventual`
  default, `latest` opt-in).
- **Global write lock on anomaly**: stalls the swarm; rejected for per-source
  advisory hold.
- **Reuse verification-drift/pack-replay/freshness for consistency**: those
  identify drift/replay state, not a per-response coherence contract; rejected as
  insufficient (read-fence is distinct).

## Verification

- Read-fence unit + property (bd-1n0np.8.4): generation monotonicity under
  interleaved writes; `latest` fails-closed on behind-asset; snapshot replay
  coherent; degraded-code fixtures.
- Write-immune unit + property (bd-1n0np.8.8): non-anomalous source never
  quarantined and the global path never stalls under randomized interleavings;
  anomalous burst always held.
- e2e `scripts/e2e_read_fence.sh` (multi-generation logging) and
  `scripts/e2e_write_immune.sh` (multi-source burst logging).
- Perf budget (bd-1n0np.8.12): default consistency-block overhead negligible.
- Golden-churn risk: the consistency block is additive + stable-ordered and
  goldens are updated in one coordinated change (bd-1n0np.8.3 note).
