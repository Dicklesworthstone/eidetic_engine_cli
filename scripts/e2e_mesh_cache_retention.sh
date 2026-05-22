#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/mesh_e2e_outcomes.sh
source "$SCRIPT_DIR/lib/mesh_e2e_outcomes.sh"

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

mesh_e2e_emit_note "$surface" "matrix" "mesh cache retention fixture loaded"
for scenario in "${scenarios[@]}"; do
  mesh_e2e_emit_scheduled "$surface" "$scenario"
done

missing_terms=()
for required in \
  derived_peer_cache \
  local_source_truth \
  cache_bytes_before \
  cache_bytes_after \
  evicted_count \
  mesh.cache.evict
do
  if ! grep -Fq "$required" docs/mesh/cache_retention.md; then
    missing_terms+=("$required")
  fi
done
if [ "${#missing_terms[@]}" -gt 0 ]; then
  mesh_e2e_emit_failed "$surface" "mesh cache retention docs missing required terms: ${missing_terms[*]}" "${scenarios[@]}"
  exit 1
fi

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  mesh_e2e_emit_skipped "$surface" "RCH binary not found; refusing to run mesh cache retention cargo test locally" "${scenarios[@]}"
  exit 2
fi

mesh_e2e_run_with_outcomes "$surface" "${scenarios[@]}" -- \
  env RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --test mesh_cache -- --nocapture
