#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SCHEMA_DIR="$REPO_ROOT/docs/schemas/swarm"
FIXTURE="$REPO_ROOT/tests/fixtures/swarm_schemas/all_examples.json"
EVENT_DIR="${TMPDIR:-/Volumes/USBNVME16TB/temp_agent_space/tmp}/ee-swarm-schema-events"
EVENT_LOG="$EVENT_DIR/swarm_schema_check.jsonl"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for swarm schema e2e\n' >&2
  exit 1
fi

emit_event() {
  local schema_id="$1"
  local valid="$2"
  local errors_count="$3"
  local detail="$4"
  jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg kind "swarm_schema_check" \
    --arg schema_id "$schema_id" \
    --arg detail "$detail" \
    --argjson valid "$valid" \
    --argjson errors_count "$errors_count" \
    '{schema:$schema,kind:$kind,schemaId:$schema_id,valid:$valid,errorsCount:$errors_count,detail:$detail}' \
    | tee -a "$EVENT_LOG" >&2
}

assert_fixture_filter() {
  local schema_id="$1"
  local filter="$2"
  local detail="$3"
  if jq -e "$filter" "$FIXTURE" >/dev/null; then
    emit_event "$schema_id" true 0 "$detail"
  else
    emit_event "$schema_id" false 1 "$detail"
    exit 1
  fi
}

expected_count="$(jq -r '.examples | length' "$FIXTURE")"
actual_count="$(find "$SCHEMA_DIR" -maxdepth 1 -type f -name '*.json' | wc -l | tr -d ' ')"
if [[ "$actual_count" != "$expected_count" ]]; then
  emit_event "catalog" false 1 "fixture manifest has $expected_count schema rows, found $actual_count schema files"
  exit 1
fi

for schema_file in "$SCHEMA_DIR"/*.json; do
  file_name="$(basename "$schema_file")"
  schema_id="${file_name%.json}"
  expected_id="https://eidetic-engine/schemas/swarm/$file_name"

  if ! jq -e . "$schema_file" >/dev/null; then
    emit_event "$schema_id" false 1 "invalid json"
    exit 1
  fi
  schema_dialect="$(jq -r '."$schema"' "$schema_file")"
  case "$schema_dialect" in
    "http://json-schema.org/draft-07/schema#"|"https://json-schema.org/draft/2020-12/schema") ;;
    *)
      emit_event "$schema_id" false 1 "unsupported schema dialect: $schema_dialect"
      exit 1
      ;;
  esac
  if [[ "$(jq -r '."$id"' "$schema_file")" != "$expected_id" ]]; then
    emit_event "$schema_id" false 1 "non-canonical id"
    exit 1
  fi
  if [[ "$(jq -r '.title' "$schema_file")" != "$schema_id" ]]; then
    emit_event "$schema_id" false 1 "title mismatch"
    exit 1
  fi
  if ! jq -e '.["x-ee-status"] | has("shipped") and has("tracking_bead") and has("available_in_build")' "$schema_file" >/dev/null; then
    emit_event "$schema_id" false 1 "missing x-ee-status fields"
    exit 1
  fi
  if ! jq -e --arg schema_id "$schema_id" '.examples[$schema_id] != null' "$FIXTURE" >/dev/null; then
    emit_event "$schema_id" false 1 "fixture manifest missing example"
    exit 1
  fi
  if ! jq -e '.examples | type == "array" and length > 0' "$schema_file" >/dev/null; then
    emit_event "$schema_id" false 1 "schema examples missing"
    exit 1
  fi
  if ! jq -e --arg schema_id "$schema_id" --slurpfile schema "$schema_file" \
    '.examples[$schema_id] == $schema[0].examples[0]' "$FIXTURE" >/dev/null; then
    emit_event "$schema_id" false 1 "fixture example does not match schema example"
    exit 1
  fi
  emit_event "$schema_id" true 0 "schema, status, and fixture rows present"
done

assert_fixture_filter \
  "ee.swarm.brief.v1.stalled_liveness" \
  '.examples["ee.swarm.brief.v1"].stalledBeadLiveness[0]
    | .posture == "active"
      and .action == "leave_alone"
      and .severity == "low"' \
  "swarm brief fixture includes an active liveness row"

assert_fixture_filter \
  "ee.swarm.brief.v1.stalled_liveness_postures" \
  '(.examples["ee.swarm.brief.v1"].stalledBeadLiveness | map(.posture) | sort)
    == ["active","blocked_with_evidence","human_approval_required","quiet_but_recent","reclaim_candidate","stale_needs_message"]' \
  "swarm brief fixture covers active, quiet, blocked, human-approval, stale, and reclaim liveness postures"

assert_fixture_filter \
  "ee.swarm.brief.v1.stalled_liveness_evidence" \
  '.examples["ee.swarm.brief.v1"].stalledBeadLiveness[]
    | select(.beadId == "bd-stale")
    | .posture == "reclaim_candidate"
      and .action == "reopen_manually"
      and .severity == "high"
      and (.evidenceSources | index("beads_updated_at"))
      and (.suggestedCommands | index("br update bd-stale --status open --json"))
      and (.mustNotDo | index("Do not auto-reopen in-progress work from swarm brief output."))' \
  "stalled liveness fixture carries conservative evidence and non-mutating guidance"

assert_fixture_filter \
  "ee.swarm.brief.v1.stalled_liveness_no_auto_reopen_for_protected_work" \
  '[.examples["ee.swarm.brief.v1"].stalledBeadLiveness[]
    | select(.posture == "blocked_with_evidence" or .posture == "human_approval_required" or .posture == "quiet_but_recent" or .posture == "stale_needs_message")
    | .suggestedCommands[]
    | select(contains("--status open"))]
    | length == 0' \
  "blocked, quiet, human-approval, and degraded-source liveness rows do not include reopen commands"

assert_fixture_filter \
  "ee.swarm.brief.v1.stalled_liveness_missing_mail_uncertainty" \
  '.examples["ee.swarm.brief.v1"].stalledBeadLiveness[]
    | select(.beadId == "bd-mail-degraded")
    | .posture == "stale_needs_message"
      and (.evidenceSources | index("agent_mail_degraded"))
      and (.evidence | index("source_status:agent_mail:not_ready"))
      and (.mustNotDo | index("Do not treat missing Agent Mail data as inactivity proof."))' \
  "missing Agent Mail liveness evidence stays uncertain rather than reclaiming"

assert_fixture_filter \
  "ee.swarm.brief.v1.agent_mail_roster_liveness" \
  '.examples["ee.swarm.brief.v1"].agentMailAgents[0]
    | .name == "OtherAgent"
      and .lastActiveAt == "2026-05-15T15:45:00Z"' \
  "swarm brief fixture includes redacted Agent Mail roster activity"

assert_fixture_filter \
  "ee.support_bundle.swarm_brief_summary.v1.stalled_liveness_summary" \
  '.examples["ee.support_bundle.swarm_brief_summary.v1"]
    | .counts.stalledBeadLivenessCount == 6
      and .counts.agentMailAgentCount == 1
      and .stalledBeadLivenessSummary.countsByPosture.active == 1
      and .stalledBeadLivenessSummary.countsByPosture.blocked_with_evidence == 1
      and .stalledBeadLivenessSummary.countsByPosture.human_approval_required == 1
      and .stalledBeadLivenessSummary.countsByPosture.quiet_but_recent == 1
      and .stalledBeadLivenessSummary.countsByPosture.reclaim_candidate == 1
      and .stalledBeadLivenessSummary.countsByPosture.stale_needs_message == 1
      and .stalledBeadLivenessSummary.topInProgressBeads[0].rawEvidenceIncluded == false
      and .stalledBeadLivenessSummary.topInProgressBeads[0].rawCommandsIncluded == false
      and .redaction.rawAgentNamesIncluded == false
      and .redaction.recommendationEvidenceIncluded == "hashes_only"' \
  "support-bundle summary keeps stalled liveness counts and evidence redacted"

assert_fixture_filter \
  "ee.swarm.brief.v1.stalled_liveness_redaction" \
  '(.examples["ee.swarm.brief.v1"] | tojson | contains("body_md") | not)
    and (.examples["ee.support_bundle.swarm_brief_summary.v1"] | tojson | contains("raw secret body") | not)
    and (.examples["ee.support_bundle.swarm_brief_summary.v1"].stalledBeadLivenessSummary.topInProgressBeads[0].titleHash | startswith("blake3:"))' \
  "stalled liveness fixtures avoid raw mail bodies and hash support-bundle titles"

printf 'swarm schema e2e passed; events=%s\n' "$EVENT_LOG" >&2
