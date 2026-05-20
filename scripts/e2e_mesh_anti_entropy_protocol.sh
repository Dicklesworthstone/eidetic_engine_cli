#!/usr/bin/env bash
set -euo pipefail

scenarios=(
  tip_advertise_builds_bounded_range_requests
  cursor_advances_only_after_durable_contiguous_replay
  range_digest_is_order_independent
  bounded_retry_blocks_after_max_attempts
  sync_summary_is_redaction_safe
  two_peer_partition_rejoin_converges
)

for scenario in "${scenarios[@]}"; do
  printf '{"schema":"ee.test_event.v1","surface":"mesh_anti_entropy_protocol","scenario":"%s","stage":"scheduled"}\n' "$scenario"
done

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  printf 'RCH binary not found; refusing to run mesh anti-entropy protocol cargo test locally\n' >&2
  exit 2
fi

RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --test mesh_anti_entropy_protocol -- --nocapture
