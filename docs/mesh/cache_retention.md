# Mesh Cache Retention

SRR6 mesh cache rows are derived peer material. Cache eviction may remove
metadata rows, fetched bodies, embeddings, or graph-adjacent derived artifacts
that were imported from peers, but it must not delete or rewrite local
source-of-truth memories.

## Boundaries

- `derived_peer_cache` is quota-managed and evictable.
- `local_source_truth` is not counted against mesh cache quotas and is never an
  eviction target.
- A peer body that fails content-hash validation moves to `quarantined` and is
  not persisted as an available body.
- `expired` cache entries are evicted before ordinary score/LRU quota pressure.

## Quotas

The retention model tracks:

- total mesh cache bytes,
- per-peer bytes,
- metadata bytes,
- body bytes,
- embedding bytes.

Eviction order is deterministic: expired entries first, then lower retention
score, older last-access sequence, body before embedding before metadata, larger
byte size, peer id, and cache key. This preserves metadata longer than bulk
bodies when both are otherwise equally disposable.

## Audit Fields

Every eviction decision carries the structured fields operators need to inspect
quota pressure:

- `cache_bytes_before`
- `cache_bytes_after`
- `evicted_count`
- `peer_id`
- `reason`

The audit action for eviction is `mesh.cache.evict`; manual purge callers must
use the same boundary rule and emit `mesh.cache.purge`.

## Eager Replication

Before eager body or embedding replication, callers should run the admission
warning check. A warning is emitted when the candidate would exceed a global,
per-peer, or lane quota, or when it would cross the near-limit threshold. The
warning is advisory; the actual cleanup decision remains the retention plan.
