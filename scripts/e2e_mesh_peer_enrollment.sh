#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/mesh_e2e_outcomes.sh
source "$SCRIPT_DIR/lib/mesh_e2e_outcomes.sh"

surface="mesh_peer_enrollment"
scenarios=(
  pair
  deny
  rotate
  revoke
  unknown_peer
)

printf '{"schema":"ee.test_event.v1","surface":"%s","phase":"setup","scenario":"matrix","message":"peer enrollment fixture loaded"}\n' "$surface"
for scenario in "${scenarios[@]}"; do
  case "$scenario" in
    pair) command='ee mesh peer add --profile body-allowed --yes --json' ;;
    deny) command='ee mesh peer add --profile body-allowed --responder-capability mesh:metadata --yes --json' ;;
    rotate) command='ee mesh peer rotate peer_... --public-key-fingerprint blake3:rotated --json' ;;
    revoke) command='ee mesh peer revoke peer_... --json' ;;
    unknown_peer) command='ee mesh peer unknown-attempt --tailscale-node-key nodekey:unknown --json' ;;
  esac
  mesh_e2e_emit_scheduled "$surface" "$scenario" "$command"
done

if [[ ! -x scripts/rch_verify.sh ]]; then
  mesh_e2e_emit_skipped "$surface" "scripts/rch_verify.sh not found; refusing to run mesh peer enrollment cargo test locally" "${scenarios[@]}"
  exit 2
fi

mesh_e2e_run_with_outcomes "$surface" "${scenarios[@]}" -- \
  env RCH_REQUIRE_REMOTE=1 scripts/rch_verify.sh --bead-id bd-1x87h --summary --no-write -- cargo test --test mesh_peer_enrollment -- --nocapture
