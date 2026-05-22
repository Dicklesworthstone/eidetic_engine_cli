#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/mesh_e2e_outcomes.sh
source "$SCRIPT_DIR/lib/mesh_e2e_outcomes.sh"

surface="mesh_replay_convergence"
scenarios=(
  event_hash_and_range_summary_are_deterministic
  missed_ranges_and_out_of_order_batches
  partition_then_rejoin_converges
  conflicting_revisions_are_explicit
  tombstone_and_validity_propagate
  peer_restart_rehydrates_durable_log
)

for scenario in "${scenarios[@]}"; do
  mesh_e2e_emit_scheduled "$surface" "$scenario"
done

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  mesh_e2e_emit_skipped "$surface" "RCH binary not found; refusing to run mesh replay convergence cargo test locally" "${scenarios[@]}"
  exit 2
fi

mesh_e2e_run_with_outcomes "$surface" "${scenarios[@]}" -- \
  env RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --test mesh_replay_convergence -- --nocapture
