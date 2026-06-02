#!/usr/bin/env bash
# Fixture-backed E2E smoke for bd-ppbue.10 ready-work reservation pressure.
# This script does not invoke Cargo or mutate the repo; it validates the
# committed swarm brief fixture and logs command/size/timing/action evidence.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURE="$REPO_ROOT/docs/schemas/swarm/ee.swarm.brief.v1.json"
EVENT_DIR="${TMPDIR:-/tmp}/ee-reservation-pressure-events"
EVENT_LOG="$EVENT_DIR/reservation_pressure.jsonl"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for reservation pressure e2e\n' >&2
  exit 1
fi

emit_event() {
  local assertion="$1"
  local passed="$2"
  local detail="$3"
  local command="$4"
  local byte_count="$5"
  local elapsed_ms="$6"
  local conflict_count="$7"
  local recommended_action="$8"
  jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg kind "ready_reservation_pressure" \
    --arg assertion "$assertion" \
    --arg detail "$detail" \
    --arg command "$command" \
    --arg recommended_action "$recommended_action" \
    --argjson passed "$passed" \
    --argjson byte_count "$byte_count" \
    --argjson elapsed_ms "$elapsed_ms" \
    --argjson conflict_count "$conflict_count" \
    '{
      schema:$schema,
      kind:$kind,
      assertion:$assertion,
      passed:$passed,
      detail:$detail,
      command:$command,
      byteCount:$byte_count,
      elapsedMs:$elapsed_ms,
      conflictCount:$conflict_count,
      recommendedAction:$recommended_action
    }' | tee -a "$EVENT_LOG" >&2
}

assert_filter() {
  local assertion="$1"
  local filter="$2"
  local detail="$3"
  local command='jq .examples[0].readyReservationPressure docs/schemas/swarm/ee.swarm.brief.v1.json'
  local start_ns
  start_ns="$(date +%s%N)"
  local byte_count
  byte_count="$(jq -c '.examples[0].readyReservationPressure' "$FIXTURE" | wc -c | tr -d ' ')"
  local conflict_count
  conflict_count="$(jq '.examples[0].readyReservationPressure | length' "$FIXTURE")"
  local recommended_action
  recommended_action="$(jq -r '.examples[0].readyReservationPressure[0].action // ""' "$FIXTURE")"
  if jq -e "$filter" "$FIXTURE" >/dev/null; then
    local elapsed_ms=$(( ( $(date +%s%N) - start_ns ) / 1000000 ))
    emit_event "$assertion" true "$detail" "$command" "$byte_count" "$elapsed_ms" "$conflict_count" "$recommended_action"
  else
    local elapsed_ms=$(( ( $(date +%s%N) - start_ns ) / 1000000 ))
    emit_event "$assertion" false "$detail" "$command" "$byte_count" "$elapsed_ms" "$conflict_count" "$recommended_action"
    exit 1
  fi
}

assert_filter \
  "ready_bead_conflict_present" \
  '.examples[0].readyReservationPressure | length == 1' \
  "fixture has one ready bead blocked by a reservation"

assert_filter \
  "exclusive_wait_action" \
  '.examples[0].readyReservationPressure[0] | .exclusiveReservationCount == 1 and .sharedReservationCount == 0 and .action == "wait"' \
  "active exclusive reservation recommends wait"

assert_filter \
  "holder_and_surface_reported" \
  '.examples[0].readyReservationPressure[0] | (.reservationHolders | index("OtherAgent")) and (.likelySurfaces | index("src/core/swarm_brief.rs"))' \
  "holder and likely surface are present without raw mail bodies"

assert_filter \
  "next_steps_are_conservative" \
  '.examples[0].readyReservationPressure[0].suggestedCommands | index("message OtherAgent before editing src/core/swarm_brief.rs")' \
  "suggested command coordinates with the holder"

if grep -qE 'body_md|raw secret body|ghp_' "$EVENT_LOG"; then
  printf 'error: reservation pressure event log leaked raw mail or token text\n' >&2
  exit 1
fi

printf 'reservation pressure e2e passed; events=%s\n' "$EVENT_LOG" >&2
