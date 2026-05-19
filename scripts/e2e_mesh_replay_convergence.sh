#!/usr/bin/env bash
set -euo pipefail

scenarios=(
  event_hash_and_range_summary_are_deterministic
  missed_ranges_and_out_of_order_batches
  partition_then_rejoin_converges
  conflicting_revisions_are_explicit
  tombstone_and_validity_propagate
  peer_restart_rehydrates_durable_log
)

for scenario in "${scenarios[@]}"; do
  printf '{"schema":"ee.test_event.v1","surface":"mesh_replay_convergence","scenario":"%s","phase":"setup","message":"scheduled"}\n' "$scenario"
done

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  printf 'RCH binary not found; refusing to run mesh replay convergence cargo test locally\n' >&2
  exit 2
fi

RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --test mesh_replay_convergence -- --nocapture
