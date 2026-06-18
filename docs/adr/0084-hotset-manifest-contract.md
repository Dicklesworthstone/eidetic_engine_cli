# ADR 0084: Read-Only Hotset Manifest Contract

Status: proposed
Date: 2026-06-18
Bead: bd-ty3pl.1

## Context

Swarm-scale sessions repeatedly discover the same high-value repo paths,
memories, search shards, graph neighborhoods, read-pool targets, pack L2
candidates, and lexical RAM-tier candidates. Later work can prewarm those
derived assets, but the project needs a contract before any implementation can
decide what is safe to consider.

The risk is not only performance. A hotset surface that claims work, sends
mail, repairs coordination state, or silently warms stale derived assets would
become an implicit scheduler and would violate the local-first CLI boundary.
The manifest also has to be safe for support bundles and handoffs: it must not
store raw mail bodies, raw memory content, raw query text, host-private absolute
paths, or expanded file listings.

Existing contracts cover adjacent concerns:

- ADR 0017 defines resource governance and rejects scheduler behavior.
- ADR 0027 defines the read-only swarm brief and its privacy posture.
- `ee.source_authority.snapshot.v1` records per-source freshness and authority.
- `ee.resource_admission.v1` decides whether a workload profile is admissible.
- `ee.pack.slo.v1` and replay scorecards record pack/replay SLO evidence.
- `ee.cache.hotset.v1` records cache prewarm entries after a hotset is already
  known.
- `ee.status.search.lexical_ram_tier.v1` reports lexical RAM-tier posture.

The missing piece is a redaction-safe, read-only plan manifest that explains
which candidates were selected, why they are fresh enough to consider, what
evidence supports them, what resource profile they belong to, and what would
invalidate them.

## Decision

`ee.hotset_manifest.v1` is the normative hotset manifest contract:
[`docs/schemas/ee.hotset_manifest.v1.json`](../schemas/ee.hotset_manifest.v1.json).

The manifest is a plan surface, not an execution surface. It is registered in
the public schema registry so agents and tests can export the contract before
collector or prewarm execution work lands.

The manifest records:

| Area | Purpose |
| --- | --- |
| Manifest identity | Stable manifest id, generated timestamp, workspace label, workspace hash, and provenance hash. |
| Source snapshots | Redaction-safe hashes and freshness for Beads/BV, Agent Mail, git/workspace hygiene, RCH, source authority, resource admission, replay/SLO, search/index, graph, read-pool, pack L2, and lexical RAM-tier evidence. |
| Item categories | Closed item classes for paths, memories, search/index shards, graph neighborhoods, read-pool targets, pack L2 candidates, and lexical RAM-tier candidates. |
| Evidence references | Bounded subject previews, source schema ids, evidence ids, and hashes only. |
| Freshness and confidence | Per-item freshness, generation, age budget, confidence score, and reason codes. |
| Resource class | Tiny, small, standard, swarm-heavy, or unsupported admission posture for each item and for the manifest as a whole. |
| Invalidation | Closed reasons that make the manifest or an item fail closed rather than authorize warming. |

The manifest-level redaction posture is
`hashes_counts_bounded_labels_no_content`. Item labels and evidence subjects are
bounded, repo-relative or symbolic labels; host-private absolute paths and raw
coordination bodies are forbidden by contract.

## Fail-Closed Behavior

The manifest may be emitted with zero items or with invalidated items, but it
must not authorize prewarm execution when authority is incomplete. The following
conditions set `failClosed.status = "fail_closed"` or mark affected items with a
non-`none` `invalidationReason`:

- stale Beads or BV evidence (`beads_stale`, `source_snapshot_stale`)
- degraded, corrupt, stale, or unavailable Agent Mail evidence
  (`agent_mail_degraded`)
- dirty checkout overlap with the candidate path or shard
  (`dirty_checkout_overlap`)
- missing or blocked RCH proof where the selected resource profile requires
  remote proof (`rch_proof_missing`)
- unsupported resource profile (`unsupported_resource_profile`)
- generation mismatch between source snapshots and derived search/graph/cache
  assets (`generation_mismatch`)
- missing or contradictory evidence (`evidence_missing`, `contradicted`)
- redaction or privacy failure (`redaction_violation`)

Failing closed means the manifest remains useful as diagnostic evidence, but a
prewarm executor must skip execution until a fresh manifest clears the blocker.

## Relationship To Existing Work

- **Resource admission** consumes the manifest's resource class and may refuse,
  queue, split, or degrade a future prewarm workload. The manifest does not make
  that decision itself.
- **Source-authority snapshots** supply the freshness and authority evidence.
  The manifest stores hashes and compact source states so consumers can tell
  whether the evidence was live, stale fallback, degraded read-only, timed out,
  or contradicted.
- **Replay and SLO contracts** explain whether a chosen hotset shape is worth
  warming for a profile. The manifest can cite replay/SLO evidence; it does not
  run replay labs or benchmarks.
- **Lexical RAM-tier** posture can nominate RAM-tier candidates and explain why
  they are safe to pin. The manifest records the candidate and evidence; the RAM
  tier still owns actual admission and memory pressure behavior.
- **`ee.cache.hotset.v1`** remains the lower-level cache-entry manifest used by
  cache prewarm reports. `ee.hotset_manifest.v1` is the higher-level plan that
  explains why paths, memories, shards, graph neighborhoods, and resource
  targets belong in a hotset before execution.

## Constraints

- Read-only: creating or exporting this manifest must not claim Beads, reserve
  files, send or acknowledge Agent Mail, run Cargo/RCH, mutate caches, mutate
  memory, mutate git, or prewarm anything.
- Local-first and CLI-first: no daemon, web service, network dependency, or
  scheduler is required to interpret the manifest.
- Redaction-safe: raw mail bodies, raw memory bodies, raw query text, raw
  command argv, environment dumps, host-private absolute paths, and full file
  listings are excluded.
- Deterministic: producers sort source snapshots, items, evidence refs, and
  degraded entries before hashing.
- Support-bundle safe: the manifest is suitable for shared diagnostics after
  ordinary support-bundle redaction.

## Rejected Alternatives

- **Make hotset collection a scheduler.** Rejected. Agent harnesses and Beads
  own work selection; this manifest only describes read-only prewarm candidates.
- **Directly execute prewarm from the schema slice.** Rejected. Execution needs
  separate admission, freshness, mutation-audit, and RCH-proof work.
- **Reuse `ee.cache.hotset.v1` for every candidate class.** Rejected. Cache
  hotsets are specific to search and pack cache entries; this manifest also
  needs source authority, path, memory, graph, read-pool, pack L2, lexical RAM,
  and invalidation evidence.
- **Store raw paths, queries, memory snippets, or mail subjects.** Rejected.
  Bounded labels, previews, and hashes are enough for explainability without
  leaking private content.
- **Treat stale evidence as merely lower confidence.** Rejected. Stale
  coordination or RCH evidence can make execution unsafe; consumers must fail
  closed instead of warming from stale authority.

## Verification

- `docs/schemas/ee.hotset_manifest.v1.json` pins the schema id, redaction
  posture, item-class vocabulary, resource-class vocabulary, freshness states,
  and invalidation reasons.
- `src/models/schema.rs` includes `ee.hotset_manifest.v1` in `KNOWN_SCHEMAS`.
- `src/output/mod.rs::public_schemas()` exports the schema through
  `ee schema list` and `ee schema export`.
- The schema includes representative cold, hot, and degraded examples that
  validate against the contract.
- Later collector/prewarm beads must add runtime fixtures proving that manifest
  production remains read-only and that stale Beads, degraded Agent Mail, dirty
  checkout overlap, missing RCH proof, and unsupported resource profiles all
  fail closed.
