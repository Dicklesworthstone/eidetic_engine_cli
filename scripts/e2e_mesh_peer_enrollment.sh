#!/usr/bin/env bash
set -euo pipefail

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
  printf '{"schema":"ee.test_event.v1","surface":"%s","phase":"assert","scenario":"%s","command":"%s","stage":"scheduled"}\n' "$surface" "$scenario" "$command"
done

if [[ ! -x scripts/rch_verify.sh ]]; then
  printf 'scripts/rch_verify.sh not found; refusing to run mesh peer enrollment cargo test locally\n' >&2
  exit 2
fi

RCH_REQUIRE_REMOTE=1 scripts/rch_verify.sh --bead-id bd-1x87h --summary --no-write -- cargo test --test mesh_peer_enrollment -- --nocapture
