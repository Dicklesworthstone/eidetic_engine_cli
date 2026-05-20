#!/usr/bin/env bash
set -euo pipefail

surface="mesh_selective_sync"
fixture="tests/fixtures/golden/mesh/selective_sync_preview_two_peers.json"

if ! command -v jq >/dev/null 2>&1; then
  printf 'jq is required for selective sync e2e logging\n' >&2
  exit 2
fi

printf '{"schema":"ee.test_event.v1","surface":"%s","bead_id":"bd-53cus","phase":"setup","message":"selective sync fixture loaded"}\n' "$surface"
jq -c --arg surface "$surface" '
  .previews[]
  | {
      schema: "ee.test_event.v1",
      surface: $surface,
      bead_id: "bd-53cus",
      phase: "profile_preview",
      profile_id: .profileId,
      candidate_count: .candidateCount,
      allowed_count: .allowedCount,
      denied_count: .deniedCount,
      deny_reason: (.deniedByReason | keys)
    }
' "$fixture"

RCH_REQUIRE_REMOTE=1 scripts/rch_verify.sh --bead-id bd-53cus --summary --no-write -- \
  cargo test --test mesh_foreground_cli selective_sync_profile_preview_matches_golden_fixture -- --nocapture
