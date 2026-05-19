#!/usr/bin/env bash
set -euo pipefail

scenarios=(
  cursor_advances_only_after_contiguous_replay
  partition_rejoin_duplicate_out_of_order_delivery
  conflicting_revisions_are_visible
  stale_tier1_read_gets_revision_notice
  deterministic_replay_order_independent
)

for scenario in "${scenarios[@]}"; do
  printf '{"schema":"ee.test_event.v1","surface":"mesh_anti_entropy_model","scenario":"%s","stage":"scheduled"}\n' "$scenario"
done

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  printf 'RCH binary not found; refusing to run mesh anti-entropy model cargo test locally\n' >&2
  exit 2
fi

RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --test mesh_anti_entropy_model -- --nocapture
