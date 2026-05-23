#!/usr/bin/env bash
# Shared helpers for external-derivation and no-LLM reflection e2e drivers.
#
# The helpers intentionally stay in shell because the owning scripts exercise
# user-facing CLI behavior. They add derivation/reflection-specific structure on
# top of scripts/e2e_overhaul/lib/shared.sh and the ee.test_event.v1 logger.

set -o pipefail

DRH_SOURCE_MEMORY_A_ID=""
DRH_SOURCE_MEMORY_B_ID=""
DRH_EVIDENCE_SPAN_IDS="[]"
DRH_REQUEST_ID=""
DRH_REQUEST_HASH=""
DRH_CANDIDATE_ID=""
DRH_CREATED_MEMORY_ID=""
DRH_DEGRADED_CODES="[]"
DRH_RECOVERY_ACTIONS="[]"

drh_json_array_from_values() {
    python3 - "$@" <<'PY'
import json
import sys

print(json.dumps([value for value in sys.argv[1:] if value]))
PY
}

drh_hash_text() {
    local text="${1:-}"
    if command -v b3sum >/dev/null 2>&1; then
        printf '%s' "$text" | b3sum | awk '{print "blake3:" $1}'
    elif python3 -c "import blake3" >/dev/null 2>&1; then
        printf '%s' "$text" | python3 -c "import sys, blake3; print('blake3:' + blake3.blake3(sys.stdin.buffer.read()).hexdigest())"
    else
        printf '%s' "$text" | shasum -a 256 | awk '{print "sha256:" $1}'
    fi
}

drh_log_state() {
    local phase="${1:?phase required}"
    local assertion="${2:-}"
    local assertion_result="${3:-}"
    local source_memory_ids
    source_memory_ids="$(drh_json_array_from_values "$DRH_SOURCE_MEMORY_A_ID" "$DRH_SOURCE_MEMORY_B_ID")"
    _e2e_emit_event "note" \
        "phase" "$phase" \
        "workspaceId" "isolated-e2e-workspace" \
        "workspacePathRedacted" "[epic-workspace]" \
        "sourceMemoryIds" "$source_memory_ids" \
        "evidenceSpanIds" "$DRH_EVIDENCE_SPAN_IDS" \
        "candidateId" "$DRH_CANDIDATE_ID" \
        "createdMemoryId" "$DRH_CREATED_MEMORY_ID" \
        "requestId" "$DRH_REQUEST_ID" \
        "requestHash" "$DRH_REQUEST_HASH" \
        "degradedCodes" "$DRH_DEGRADED_CODES" \
        "recoveryActions" "$DRH_RECOVERY_ACTIONS" \
        "assertion" "$assertion" \
        "assertionResult" "$assertion_result"
}

drh_seed_derivation_sources() {
    local label="${1:?seed label required}"
    local memory_a memory_b

    memory_a=$(ee_workspace remember \
        "DRH ${label}: source memory A says remote Cargo proof must stay on RCH." \
        --level procedural \
        --kind rule \
        --tags derivation,reflection,e2e \
        --json 2>/dev/null || true)
    assert_jq "$memory_a" '.success // false' "true" "${label}_source_memory_a_success"
    DRH_SOURCE_MEMORY_A_ID=$(printf '%s' "$memory_a" | jq -r '.data.memory_id // .data.memoryId // empty' 2>/dev/null || true)
    assert_jq_nonempty "$memory_a" '.data.memory_id // .data.memoryId // empty' "${label}_source_memory_a_id"

    memory_b=$(ee_workspace remember \
        "DRH ${label}: source memory B says reflection results must become curation candidates, not memories." \
        --level semantic \
        --kind decision \
        --tags derivation,reflection,e2e \
        --json 2>/dev/null || true)
    assert_jq "$memory_b" '.success // false' "true" "${label}_source_memory_b_success"
    DRH_SOURCE_MEMORY_B_ID=$(printf '%s' "$memory_b" | jq -r '.data.memory_id // .data.memoryId // empty' 2>/dev/null || true)
    assert_jq_nonempty "$memory_b" '.data.memory_id // .data.memoryId // empty' "${label}_source_memory_b_id"

    DRH_REQUEST_HASH="$(drh_hash_text "${label}|${DRH_SOURCE_MEMORY_A_ID}|${DRH_SOURCE_MEMORY_B_ID}")"
    drh_log_state "${label}_sources_seeded" "source_memories_created" "ok"
}

drh_extract_degraded_codes() {
    local json="${1:-}"
    DRH_DEGRADED_CODES=$(printf '%s' "$json" | jq -c '[.degraded[]?.code, .data.degraded[]?.code] | map(select(. != null)) | unique' 2>/dev/null || printf '[]')
}

drh_extract_recovery_actions() {
    local json="${1:-}"
    DRH_RECOVERY_ACTIONS=$(printf '%s' "$json" | jq -c '[.error.details.recovery[]?.command, .data.recovery[]?.command, .data.nextActions[]?] | map(select(. != null))' 2>/dev/null || printf '[]')
}

drh_capture_candidate_id() {
    local json="${1:-}"
    DRH_CANDIDATE_ID=$(printf '%s' "$json" | jq -r '
        .data.candidateId //
        .data.candidate_id //
        .data.candidates[0].candidateId //
        .data.candidates[0].id //
        empty
    ' 2>/dev/null || true)
}

drh_capture_request_identity() {
    local json="${1:-}"
    DRH_REQUEST_ID=$(printf '%s' "$json" | jq -r '.data.requestId // .requestId // empty' 2>/dev/null || true)
    local hash
    hash=$(printf '%s' "$json" | jq -r '.data.requestHash // .requestHash // empty' 2>/dev/null || true)
    if [ -n "$hash" ]; then
        DRH_REQUEST_HASH="$hash"
    fi
}

drh_assert_test_log_contract() {
    local label="${1:?label required}"
    if [ -z "${EE_TEST_LOG_PATH:-}" ] || [ ! -f "$EE_TEST_LOG_PATH" ]; then
        e2e_log_assert_eq "missing-log" "present-log" "$label"
        return 1
    fi
    local result
    result=$(python3 - "$EE_TEST_LOG_PATH" <<'PY'
import json
import sys

path = sys.argv[1]
command_end_seen = False
state_seen = False
with open(path, "r", encoding="utf-8") as handle:
    for index, line in enumerate(handle, 1):
        if not line.strip():
            continue
        event = json.loads(line)
        if event.get("schema") != "ee.test_event.v1":
            print(f"bad schema at line {index}")
            sys.exit(0)
        if event.get("kind") == "command_end":
            command_end_seen = True
            for key in ("command", "args", "exit_code", "elapsed_ms", "stdout_hash", "stderr_hash"):
                if key not in event:
                    print(f"command_end missing {key} at line {index}")
                    sys.exit(0)
        fields = event.get("fields") or {}
        if fields.get("workspacePathRedacted") == "[epic-workspace]":
            state_seen = True
            for key in (
                "phase",
                "sourceMemoryIds",
                "evidenceSpanIds",
                "candidateId",
                "createdMemoryId",
                "requestId",
                "requestHash",
                "degradedCodes",
                "recoveryActions",
                "assertion",
                "assertionResult",
            ):
                if key not in fields:
                    print(f"state event missing {key} at line {index}")
                    sys.exit(0)
if not command_end_seen:
    print("missing command_end")
elif not state_seen:
    print("missing derivation/reflection state event")
else:
    print("ok")
PY
)
    e2e_log_assert_eq "$result" "ok" "$label"
}
