#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SNAPSHOT="$REPO_ROOT/tests/fixtures/ownership/ownership_posture_e2e_snapshot.json"
ATTRIBUTION_SNAPSHOT="$REPO_ROOT/tests/snapshots/ownership_snapshot__compile_blocker_attribution.snap"
SWARM_CASES="$REPO_ROOT/tests/fixtures/swarm/ownership_posture_cases.json"
EVENT_DIR="${EE_TEST_EVENT_DIR:-${TMPDIR:-/Volumes/USBNVME16TB/temp_agent_space/tmp}/ee-ownership-posture-events}"
EVENT_LOG="$EVENT_DIR/ownership_posture.jsonl"
AS_OF="2026-05-15T06:30:00Z"
FIRST_BLOCKER_PATH="src/graph/hits.rs"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for ownership posture e2e\n' >&2
  exit 1
fi
if ! command -v shasum >/dev/null 2>&1; then
  printf 'error: shasum is required for ownership posture e2e\n' >&2
  exit 1
fi

file_hash() {
  shasum -a 256 "$1" | awk '{print "sha256:" $1}'
}

emit_event() {
  local phase="$1"
  local valid="$2"
  local detail="$3"
  local artifact_hash="$4"
  jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg kind "ownership_posture_e2e" \
    --arg phase "$phase" \
    --arg detail "$detail" \
    --arg artifact_hash "$artifact_hash" \
    --argjson valid "$valid" \
    '{schema:$schema,kind:$kind,phase:$phase,valid:$valid,detail:$detail,artifactHash:$artifact_hash}' \
    | tee -a "$EVENT_LOG" >&2
}

fail_check() {
  local phase="$1"
  local detail="$2"
  emit_event "$phase" false "$detail" "sha256:unavailable"
  exit 1
}

assert_jq() {
  local phase="$1"
  local file="$2"
  local detail="$3"
  shift 3
  if ! jq -e "$@" "$file" >/dev/null; then
    fail_check "$phase" "$detail"
  fi
}

for required in "$SNAPSHOT" "$ATTRIBUTION_SNAPSHOT" "$SWARM_CASES"; do
  [[ -s "$required" ]] || fail_check "setup" "missing artifact $required"
done

emit_event "setup" true "fixtures present" "$(file_hash "$SNAPSHOT")"

assert_jq "snapshot" "$SNAPSHOT" \
  "ownership e2e snapshot must include reservations, beads, and dirty files" \
  '.schema == "ee.ownership_snapshot.v1" and (.reservations | length) >= 3 and (.beads | length) >= 3 and (.dirtyFiles | length) >= 3' \

assert_jq "snapshot" "$SNAPSHOT" \
  "snapshot must include active reservations" \
  --arg as_of "$AS_OF" \
  '[.reservations[] | select(.expiresAt > $as_of)] | length >= 2'

assert_jq "snapshot" "$SNAPSHOT" \
  "snapshot must include an expired reservation" \
  --arg as_of "$AS_OF" \
  '[.reservations[] | select(.expiresAt <= $as_of)] | length >= 1'

assert_jq "snapshot" "$SNAPSHOT" \
  "first compile blocker path must have an active exact reservation holder" \
  --arg path "$FIRST_BLOCKER_PATH" \
  '.reservations[] | select(.pathPattern == $path and .holderAgent == "NobleStork" and .exclusive == true)'

assert_jq "snapshot" "$SNAPSHOT" \
  "snapshot must include at least two assigned in-progress beads" \
  '[.beads[] | select(.status == "in_progress" and (.assignee != null))] | length >= 2' \

assert_jq "snapshot" "$SNAPSHOT" \
  "snapshot must include at least one open unclaimed bead" \
  '[.beads[] | select(.status == "open" and (.assignee == null))] | length >= 1' \

assert_jq "snapshot" "$SNAPSHOT" \
  "first compile blocker path must be represented in dirty files" \
  --arg path "$FIRST_BLOCKER_PATH" \
  '.dirtyFiles[] | select(.path == $path and .status == "modified")'

snapshot_text="$(jq -c . "$SNAPSHOT")"
for forbidden in body_md SECRET fileContents envDump "Bearer " "api_key" "token="; do
  if [[ "$snapshot_text" == *"$forbidden"* ]]; then
    fail_check "redaction" "ownership snapshot leaked forbidden marker $forbidden"
  fi
done

emit_event "snapshot" true "multi-agent ownership snapshot invariants passed" "$(file_hash "$SNAPSHOT")"

if ! grep -Fq '"schema": "ee.compile_blocker_attribution.v1"' "$ATTRIBUTION_SNAPSHOT"; then
  fail_check "attribution" "compile blocker attribution golden missing schema"
fi
if ! grep -Fq '"status": "attributed"' "$ATTRIBUTION_SNAPSHOT"; then
  fail_check "attribution" "compile blocker attribution golden missing attributed case"
fi
if ! grep -Fq '"status": "unattributed"' "$ATTRIBUTION_SNAPSHOT"; then
  fail_check "attribution" "compile blocker attribution golden missing unattributed case"
fi
if grep -Eiq '(body_md|fileContents|envDump|Bearer[[:space:]]+[A-Za-z0-9._-]+|api[_-]?key[[:space:]]*[:=]|token[[:space:]]*=)' "$ATTRIBUTION_SNAPSHOT"; then
  fail_check "redaction" "compile blocker attribution golden leaked raw coordination or secret-like content"
fi

emit_event "attribution" true "compile blocker attribution golden covers attributed and fallback paths" "$(file_hash "$ATTRIBUTION_SNAPSHOT")"

assert_jq "swarm_cases" "$SWARM_CASES" \
  "ownership posture fixture catalog must include required cases" \
  '.schema == "ee.swarm.ownership_posture_cases.v1" and (.cases | length) >= 3' \

for required_case in healthy_clean_checkout agent_mail_degraded_dirty_overlap unattributed_compile_blocker; do
  assert_jq "swarm_cases" "$SWARM_CASES" \
    "ownership posture fixture catalog missing $required_case" \
    --arg required_case "$required_case" \
    '.cases[] | select(.id == $required_case)'
done

assert_jq "swarm_cases" "$SWARM_CASES" \
  "compact summaries must be redaction-safe" \
  '. as $root | [.cases[].compactSummary.redaction | select(.rawMailBodiesIncluded == false and .rawQueryTextIncluded == false and .rawProvenanceTextIncluded == false and .fullFileListingsIncluded == false)] | length == ($root.cases | length)' \

emit_event "swarm_cases" true "swarm brief/support-bundle ownership posture cases passed" "$(file_hash "$SWARM_CASES")"

event_count="$(wc -l < "$EVENT_LOG" | tr -d ' ')"
if [[ "$event_count" -lt 4 ]]; then
  fail_check "summary" "expected at least four e2e test-event lines"
fi

emit_event "summary" true "ownership posture e2e checks passed" "$(file_hash "$EVENT_LOG")"
printf 'ownership posture e2e passed; events=%s\n' "$EVENT_LOG" >&2
