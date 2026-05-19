#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE="$REPO_ROOT/tests/fixtures/golden/swarm_next_action/repeated_ideawizard.json.golden"
EVENT_DIR="${TMPDIR:-/tmp}/ee-swarm-next-action-events"
EVENT_LOG="$EVENT_DIR/recommendation_cards.jsonl"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for swarm next-action recommendation-card e2e\n' >&2
  exit 1
fi

emit_event() {
  local assertion="$1"
  local passed="$2"
  local detail="$3"
  jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg kind "swarm_next_action_recommendation_card" \
    --arg assertion "$assertion" \
    --arg detail "$detail" \
    --argjson passed "$passed" \
    '{schema:$schema,kind:$kind,assertion:$assertion,passed:$passed,detail:$detail}' \
    | tee -a "$EVENT_LOG" >&2
}

require_jq() {
  local assertion="$1"
  local filter="$2"
  local detail="$3"
  if jq -e "$filter" "$FIXTURE" >/dev/null; then
    emit_event "$assertion" true "$detail"
  else
    emit_event "$assertion" false "$detail"
    exit 1
  fi
}

require_jq \
  "response_schema" \
  '.schema == "ee.response.v1" and .success == true and .data.schema == "ee.swarm_next_action.v1"' \
  "fixture is a successful swarm next-action response"

require_jq \
  "repeated_ideawizard_refines_existing_bead" \
  '.data.recommendationCards[] | select(.candidateId == "bd-3vwx0.9" and .decision == "refine_existing_bead")' \
  "bd-3vwx0.9 is refined instead of replaced by a duplicate bead"

require_jq \
  "no_duplicate_new_bead" \
  'all(.data.recommendationCards[]; (.candidateId != "bd-3vwx0.9") or (.decision != "new_bead_recommended"))' \
  "no card recommends opening a new bead for the existing SWA9 candidate"

require_jq \
  "overlap_records_existing_bead" \
  '.data.recommendationCards[] | select(.candidateId == "bd-3vwx0.9" and (.overlap.matchedExistingBeads | index("bd-3vwx0.9")) and .overlap.selectedRelation == "existing_bead")' \
  "overlap decision records bd-3vwx0.9 as the existing bead"

require_jq \
  "proof_obligations_for_closeout" \
  '.data.recommendationCards[] | select(.candidateId == "bd-3vwx0.9" and (.proofObligations | index("record_overlap_decision_in_closeout")) and (.proofObligations | index("reserve_files_before_editing")) and (.proofObligations | index("use_rch_for_cargo_verification")))' \
  "proof obligations name overlap closeout, reservations, and RCH-only verification"

printf 'swarm next-action recommendation-card e2e passed; events=%s\n' "$EVENT_LOG" >&2
