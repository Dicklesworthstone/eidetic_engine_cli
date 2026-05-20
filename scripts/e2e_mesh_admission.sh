#!/usr/bin/env bash
set -euo pipefail

surface="mesh_admission_control"
scenarios=(
  peer_throttled
  payload_rejected
  budget_exhausted
  backoff_until
  local_tier1_unaffected
)

printf '{"schema":"ee.test_event.v1","surface":"%s","phase":"setup","scenario":"matrix","message":"admission fixture loaded"}\n' "$surface"
for scenario in "${scenarios[@]}"; do
  printf '{"schema":"ee.test_event.v1","surface":"%s","phase":"assert","scenario":"%s","stage":"scheduled"}\n' "$surface" "$scenario"
done

rch_bin="${RCH_BIN:-rch}"
if ! command -v "$rch_bin" >/dev/null 2>&1; then
  printf 'RCH binary not found; refusing to run mesh admission cargo test locally\n' >&2
  exit 2
fi

RCH_REQUIRE_REMOTE=1 "$rch_bin" exec -- cargo test --lib mesh::admission -- --nocapture
