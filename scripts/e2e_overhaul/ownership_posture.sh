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
LIVE_MODE="${EE_OWNERSHIP_POSTURE_LIVE:-0}"
LIVE_FIRST_BLOCKER_PATH="${EE_OWNERSHIP_FIRST_BLOCKER_PATH:-src/core/outcome.rs}"
LIVE_RESERVATIONS_DIR="${EE_OWNERSHIP_RESERVATIONS_DIR:-$HOME/.local/share/mcp_agent_mail_rust/projects/users-jemanuel-projects-eidetic-engine-cli/file_reservations}"
LIVE_SNAPSHOT="${EE_OWNERSHIP_LIVE_SNAPSHOT:-$EVENT_DIR/live_ownership_snapshot.json}"

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

build_live_snapshot() {
  local reservations_json="$EVENT_DIR/live_reservations.json"
  local beads_raw="$EVENT_DIR/live_beads_raw.json"
  local beads_json="$EVENT_DIR/live_beads.json"
  local dirty_raw="$EVENT_DIR/live_git_status.txt"
  local dirty_json="$EVENT_DIR/live_dirty_files.json"
  local generated_at
  generated_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  [[ -d "$LIVE_RESERVATIONS_DIR" ]] \
    || fail_check "live_setup" "missing Agent Mail reservation archive $LIVE_RESERVATIONS_DIR"

  local reservation_files=()
  while IFS= read -r -d '' reservation_file; do
    reservation_files+=("$reservation_file")
  done < <(find "$LIVE_RESERVATIONS_DIR" -maxdepth 1 -type f -name '*.json' -print0 | sort -z)

  if ((${#reservation_files[@]} == 0)); then
    printf '[]\n' > "$reservations_json"
  else
    jq -s --arg project "$REPO_ROOT" '
      [
        .[]
        | select(.project == $project)
        | {
            pathPattern: .path_pattern,
            holderAgent: .agent,
            exclusive: (.exclusive // false),
            expiresAt: .expires_ts,
            beadId: (try (.reason | capture("(?<id>bd-[A-Za-z0-9][A-Za-z0-9._-]*)").id) catch null),
            threadId: (try (.reason | capture("(?<id>bd-[A-Za-z0-9][A-Za-z0-9._-]*)").id) catch null),
            provenance: {
              sourceKind: "agent_mail_reservation",
              sourceId: ("reservation-" + ((.id // .path_pattern) | tostring)),
              contentHash: ("sha256:redacted-reservation-" + ((.id // .path_pattern) | tostring))
            }
          }
      ]
      | unique_by(.provenance.sourceId)
      | sort_by(.pathPattern, .holderAgent, .expiresAt // "")
    ' "${reservation_files[@]}" > "$reservations_json"
  fi

  br list --json > "$beads_raw"
  jq '
    [
      .[]
      | {
          beadId: .id,
          title: (.title // ""),
          status: (.status // ""),
          assignee: (.assignee // null),
          labels: (.labels // []),
          filePatterns: [],
          provenance: {
            sourceKind: "beads_issue",
            sourceId: .id,
            contentHash: ("sha256:redacted-bead-" + (.id | tostring))
          }
        }
    ]
    | sort_by(.beadId)
  ' "$beads_raw" > "$beads_json"

  git status --porcelain=v1 --untracked-files=all > "$dirty_raw"
  jq -Rn '
    [
      inputs
      | select(length > 0)
      | .[3:] as $path
      | {
          path: $path,
          status: (.[0:2] | gsub(" "; "")),
          provenance: {
            sourceKind: "git_status",
            sourceId: "working_tree",
            contentHash: ("sha256:redacted-git-status-" + ($path | gsub("[^A-Za-z0-9_.-]"; "_")))
          }
        }
    ]
    | sort_by(.path)
  ' < "$dirty_raw" > "$dirty_json"

  jq -n \
    --arg schema "ee.ownership_snapshot.v1" \
    --arg generated_at "$generated_at" \
    --slurpfile reservations "$reservations_json" \
    --slurpfile beads "$beads_json" \
    --slurpfile dirty_files "$dirty_json" \
    '{
      schema: $schema,
      generatedAt: $generated_at,
      reservations: $reservations[0],
      beads: $beads[0],
      dirtyFiles: $dirty_files[0]
    }' > "$LIVE_SNAPSHOT"
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

if [[ "$LIVE_MODE" == "1" ]]; then
  build_live_snapshot
  emit_event "live_setup" true "live ownership snapshot generated" "$(file_hash "$LIVE_SNAPSHOT")"

  assert_jq "live_snapshot" "$LIVE_SNAPSHOT" \
    "live snapshot must include actual reservations, beads, and dirty files" \
    '.schema == "ee.ownership_snapshot.v1" and (.reservations | length) >= 1 and (.beads | length) >= 1 and (.dirtyFiles | length) >= 1'

  assert_jq "live_snapshot" "$LIVE_SNAPSHOT" \
    "live snapshot must include both active and historical reservations" \
    --arg as_of "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '([.reservations[] | select(.expiresAt > $as_of)] | length >= 1) and ([.reservations[] | select(.expiresAt <= $as_of)] | length >= 1)'

  assert_jq "live_snapshot" "$LIVE_SNAPSHOT" \
    "live snapshot must include claimed in-progress and open unclaimed beads" \
    '([.beads[] | select(.status == "in_progress" and (.assignee != null))] | length >= 1) and ([.beads[] | select(.status == "open" and (.assignee == null))] | length >= 1)'

  assert_jq "live_snapshot" "$LIVE_SNAPSHOT" \
    "live first compile blocker must have an active exclusive reservation holder" \
    --arg path "$LIVE_FIRST_BLOCKER_PATH" \
    --arg as_of "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '[.reservations[] | select(.pathPattern == $path and .exclusive == true and (.expiresAt > $as_of))] | length >= 1'

  assert_jq "live_snapshot" "$LIVE_SNAPSHOT" \
    "live first compile blocker path must be represented in dirty files" \
    --arg path "$LIVE_FIRST_BLOCKER_PATH" \
    '.dirtyFiles[] | select(.path == $path)'

  live_snapshot_text="$(jq -c . "$LIVE_SNAPSHOT")"
  for forbidden in body_md SECRET fileContents envDump "Bearer " "api_key" "token="; do
    if [[ "$live_snapshot_text" == *"$forbidden"* ]]; then
      fail_check "live_redaction" "live ownership snapshot leaked forbidden marker $forbidden"
    fi
  done

  emit_event "live_snapshot" true "live ownership snapshot invariants passed" "$(file_hash "$LIVE_SNAPSHOT")"
fi

event_count="$(wc -l < "$EVENT_LOG" | tr -d ' ')"
if [[ "$event_count" -lt 4 ]]; then
  fail_check "summary" "expected at least four e2e test-event lines"
fi

emit_event "summary" true "ownership posture e2e checks passed" "$(file_hash "$EVENT_LOG")"
printf 'ownership posture e2e passed; events=%s\n' "$EVENT_LOG" >&2
