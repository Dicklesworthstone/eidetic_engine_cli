#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="$REPO_ROOT/tests/fixtures/golden/swarm_next_action"
REPEATED_IDEAWIZARD_FIXTURE="$FIXTURE_DIR/repeated_ideawizard.json.golden"
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

FIXTURES=()
while IFS= read -r fixture; do
  FIXTURES+=("$fixture")
done < <(find "$FIXTURE_DIR" -maxdepth 1 -type f -name '*.json.golden' -print | sort)

if [ "${#FIXTURES[@]}" -eq 0 ]; then
  printf 'error: no swarm next-action golden fixtures found in %s\n' "$FIXTURE_DIR" >&2
  exit 1
fi

for FIXTURE in "${FIXTURES[@]}"; do
  fixture_name="$(basename "$FIXTURE")"
  require_jq \
    "response_schema:$fixture_name" \
    '(.schema == "ee.response.v2" or .schema == "ee.response.v1") and .success == true and .data.schema == "ee.swarm_next_action.v1"' \
    "$fixture_name is a successful swarm next-action response envelope"

  require_jq \
    "recommendation_cards_present:$fixture_name" \
    '(.data.recommendationCards | type == "array") and (.data.recommendationCards | length > 0)' \
    "$fixture_name includes at least one recommendation card"

  # shellcheck disable=SC2016 # $paths is a jq variable, not a shell variable.
  require_jq \
    "no_duplicate_exclusive_reservation_paths:$fixture_name" \
    '[.data.recommendationCards[].suggestedReservations[]? | select(.exclusive == true) | .pathPattern] as $paths | ($paths | length) == ($paths | unique | length)' \
    "$fixture_name has no overlapping exclusive reservation paths"
done

FIXTURE="$FIXTURE_DIR/dirty.json.golden"
require_jq \
  "dirty_checkout_blocks_owner" \
  '.data.recommendationCards[] | select(.candidateId == "bd-dirty" and .decision == "blocked_by_owner" and (.suggestedReservations | length == 0) and (.doNotTakeBecause | index("dirty_compile_health_blocks_rch")))' \
  "dirty checkout fixture blocks owner-conflicting work and avoids reservations"

FIXTURE="$FIXTURE_DIR/degraded_beads.json.golden"
require_jq \
  "degraded_beads_repairs_sources_first" \
  '.data.recommendationCards[] | select(.decision == "no_action_recommended" and (.proofObligations | index("repair_degraded_sources_before_creating_tracker_work")) and .fallbackDecision == "repair_evidence_providers")' \
  "degraded Beads fixture recommends repairing evidence providers before tracker mutation"

FIXTURE="$FIXTURE_DIR/degraded_mail.json.golden"
require_jq \
  "degraded_mail_still_uses_beads_fallback" \
  '.data.recommendationCards[] | select(.candidateId == "bd-mail" and .decision == "refine_existing_bead" and (.evidenceCaveats | index("degraded:agent_mail:agent_mail_unavailable")))' \
  "degraded Agent Mail fixture preserves Beads fallback evidence"

FIXTURE="$FIXTURE_DIR/saturated_rch.json.golden"
require_jq \
  "saturated_rch_marks_remote_only_unsafe" \
  '.data.recommendationCards[] | select(.candidateId == "bd-rch" and (.evidenceCaveats | index("remote_only_rch_not_safe")) and (.proofObligations | index("use_rch_for_cargo_verification")))' \
  "saturated RCH fixture keeps remote-only verification caveat and RCH proof obligation"

FIXTURE="$FIXTURE_DIR/convoy_rch.json.golden"
require_jq \
  "convoy_rch_marks_head_of_line" \
  '.data.recommendationCards[] | select(.candidateId == "bd-rch" and (.evidenceCaveats | index("rch_head_of_line_blocked")) and (.evidenceCaveats | index("remote_only_rch_not_safe")))' \
  "convoy RCH fixture records head-of-line and remote-only caveats"

FIXTURE="$REPEATED_IDEAWIZARD_FIXTURE"

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
