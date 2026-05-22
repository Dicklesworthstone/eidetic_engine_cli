#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/mesh_e2e_outcomes.sh
source "$SCRIPT_DIR/lib/mesh_e2e_outcomes.sh"

surface="mesh_anti_entropy_protocol"
scenarios=(
  tip_advertise_builds_bounded_range_requests
  cursor_advances_only_after_durable_contiguous_replay
  range_digest_is_order_independent
  bounded_retry_blocks_after_max_attempts
  sync_summary_is_redaction_safe
  two_peer_partition_rejoin_converges
)

for scenario in "${scenarios[@]}"; do
  mesh_e2e_emit_scheduled "$surface" "$scenario"
done

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  mesh_e2e_emit_skipped "$surface" "RCH binary not found; refusing to run mesh anti-entropy protocol cargo test locally" "${scenarios[@]}"
  exit 2
fi

mesh_e2e_run_with_outcomes "$surface" "${scenarios[@]}" -- \
  env RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --test mesh_anti_entropy_protocol -- --nocapture
