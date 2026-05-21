#!/usr/bin/env bash
set -euo pipefail

scenarios=(
  cursor_advances_only_after_contiguous_replay
  partition_rejoin_duplicate_out_of_order_delivery
  conflicting_revisions_are_visible
  stale_tier1_read_gets_revision_notice
  deterministic_replay_order_independent
  withdrawal_propagates_as_provenance_tombstone
  withdrawn_remote_material_renders_search_context_why_contract
  validity_expiry_filters_without_peer_cache_purge
  tombstone_hides_from_search_without_body_purge
  withdrawal_wins_over_tombstone_and_validity_expiry
  malformed_hash_body_policy_schema_events_enter_quarantine
  crash_after_insert_before_cursor_requires_repair
  quarantine_repair_actions_are_audited
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
