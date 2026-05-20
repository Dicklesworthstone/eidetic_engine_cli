#!/usr/bin/env bash
set -euo pipefail

surface="mesh_cache_retention"
scenarios=(
  peer_body_sync_until_global_quota
  per_peer_quota_eviction
  body_lane_quota_preserves_metadata
  expired_body_evicted_first
  content_hash_mismatch_quarantine
  eager_replication_warning
  local_source_truth_protected
)

printf '{"schema":"ee.test_event.v1","surface":"%s","phase":"setup","scenario":"matrix","message":"mesh cache retention fixture loaded"}\n' "$surface"
for scenario in "${scenarios[@]}"; do
  printf '{"schema":"ee.test_event.v1","surface":"%s","phase":"assert","scenario":"%s","stage":"scheduled"}\n' "$surface" "$scenario"
done

for required in \
  derived_peer_cache \
  local_source_truth \
  cache_bytes_before \
  cache_bytes_after \
  evicted_count \
  mesh.cache.evict
do
  if ! grep -Fq "$required" docs/mesh/cache_retention.md; then
    printf 'mesh cache retention docs missing required term: %s\n' "$required" >&2
    exit 1
  fi
done

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  printf 'RCH binary not found; refusing to run mesh cache retention cargo test locally\n' >&2
  exit 2
fi

RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --test mesh_cache -- --nocapture
