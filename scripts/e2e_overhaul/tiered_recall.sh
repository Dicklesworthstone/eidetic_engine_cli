#!/usr/bin/env bash
# bd-1prrl.6.5 - Tiered hot/warm/cold recall e2e and perf evidence.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

export EE_E2E_KEEP_ARTIFACTS="${EE_E2E_KEEP_ARTIFACTS:-1}"
export EE_E2E_KEEP_WORKSPACE="${EE_E2E_KEEP_WORKSPACE:-1}"
export EE_E2E_ALLOW_WORKSPACE_DELETE=0

require_jq

START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "tiered_recall"

QUERY="tiered recall release cold explicit failure evidence"
FILLER_COUNT="${EE_TIERED_RECALL_FILLER_COUNT:-650}"
PERF_REPEATS="${EE_TIERED_RECALL_PERF_REPEATS:-3}"
CANDIDATE_POOL=$((FILLER_COUNT + 1))
RECORDS_PATH="$EPIC_WORKSPACE/tiered_recall_records.jsonl"
PERF_SAMPLES="$EPIC_WORKSPACE/tiered_recall_perf_samples.txt"

e2e_log_assert_num "$FILLER_COUNT" -ge 641 "tiered_recall_corpus_large_enough_for_cold_tier"
e2e_log_assert_num "$PERF_REPEATS" -ge 1 "tiered_recall_perf_repeats_positive"

write_pack_config() {
    local enabled="${1:?enabled boolean required}"
    mkdir -p "$EPIC_WORKSPACE/.ee"
    printf '[pack]\nmemory_tier_admission = %s\n' "$enabled" >"$EPIC_WORKSPACE/.ee/config.toml"
}

hash_json_projection() {
    local json="${1:-}"
    local filter="${2:?jq filter required}"
    printf '%s' "$json" \
        | jq -cS "$filter" 2>/dev/null \
        | shasum -a 256 \
        | awk '{print "sha256:" $1}'
}

emit_tiered_recall_event() {
    local status="$1"
    local disabled_hash="$2"
    local explicit_disabled_hash="$3"
    local enabled_hash="$4"
    local parity="$5"
    local tier_boosted="$6"
    local tier_cold="$7"
    local tier_required_cold="$8"
    local p50_ms="$9"
    local p95_ms="${10}"
    local timing_present="${11}"
    local cold_selected="${12}"
    local elapsed_ms="${13}"

    [ -z "${EE_TEST_LOG_PATH:-}" ] && return 0
    python3 - "$EE_TEST_LOG_PATH" "$status" "$disabled_hash" "$explicit_disabled_hash" \
        "$enabled_hash" "$parity" "$tier_boosted" "$tier_cold" "$tier_required_cold" \
        "$p50_ms" "$p95_ms" "$timing_present" "$cold_selected" "$elapsed_ms" <<'PY'
import json
import os
import sys
from datetime import datetime, timezone

(
    log_path,
    status,
    disabled_hash,
    explicit_disabled_hash,
    enabled_hash,
    parity,
    tier_boosted,
    tier_cold,
    tier_required_cold,
    p50_ms,
    p95_ms,
    timing_present,
    cold_selected,
    elapsed_ms,
) = sys.argv[1:]

def number(value):
    try:
        if "." in value:
            return float(value)
        return int(value)
    except ValueError:
        return None

event = {
    "schema": "ee.test_event.v1",
    "ts": datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "test_id": "tiered_recall",
    "kind": "tiered_recall_perf_proof",
    "status": status,
    "exitCode": 0 if status == "passed" else 1,
    "elapsedMs": number(elapsed_ms) or 0,
    "fields": {
        "bead_id": "bd-1prrl.6.5",
        "query_shape_hash": disabled_hash,
        "disabled_output_hash": disabled_hash,
        "explicit_disabled_output_hash": explicit_disabled_hash,
        "enabled_output_hash": enabled_hash,
        "output_hash_parity": parity == "true",
        "tier_boosted_count": number(tier_boosted),
        "tier_cold_count": number(tier_cold),
        "cold_recall_count": number(tier_required_cold),
        "tier_generation": 0,
        "tier_store_status": "derived_from_memory_rows_no_separate_store",
        "cache_prewarm_effect": "advisory_only_selected_items_unchanged",
        "p50_context_latency_ms": number(p50_ms),
        "p95_context_latency_ms": number(p95_ms),
        "memory_tier_admission_timing_present": timing_present == "true",
        "required_cold_evidence_selected": cold_selected == "true",
    },
}
os.makedirs(os.path.dirname(log_path) or ".", exist_ok=True)
with open(log_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(event, sort_keys=True) + "\n")
PY
}

python3 - "$RECORDS_PATH" "$FILLER_COUNT" <<'PY'
import json
import sys

path = sys.argv[1]
filler_count = int(sys.argv[2])
export_id = "exp-tiered-recall-e2e"
created_at = "2026-05-25T00:00:00Z"

def write(handle, payload):
    handle.write(json.dumps(payload, sort_keys=True) + "\n")

with open(path, "w", encoding="utf-8") as handle:
    write(handle, {
        "schema": "ee.export.header.v1",
        "format_version": 1,
        "created_at": created_at,
        "workspace_id": "ws-tiered-recall-source",
        "workspace_path": "/synthetic/tiered-recall",
        "export_scope": "memories",
        "redaction_level": "minimal",
        "record_count": filler_count + 3,
        "ee_version": "0.3.0",
        "hostname": "tiered-recall-e2e",
        "export_id": export_id,
        "import_source": "external_import",
        "trust_level": "validated",
        "checksum": None,
        "signature": None,
        "source_schema_version": "tiered_recall_e2e.v1",
    })
    for index in range(filler_count):
        write(handle, {
            "schema": "ee.export.memory.v1",
            "memory_id": f"mem_tiered_recall_filler_{index:03d}",
            "workspace_id": "ws-tiered-recall-source",
            "level": "procedural",
            "kind": "rule",
            "content": (
                f"Tiered recall release evidence filler {index}: hot warm context for "
                "tiered recall release cold explicit failure evidence."
            ),
            "importance": 0.95,
            "confidence": 0.95,
            "utility": 0.95,
            "trust_class": "human_explicit",
            "trust_subclass": "tiered-recall-e2e",
            "created_at": created_at,
            "updated_at": created_at,
            "tombstoned_at": None,
            "tombstoned_reason": None,
            "valid_from": None,
            "valid_to": None,
            "expires_at": None,
            "source_agent": "tiered-recall-e2e",
            "provenance_uri": f"jsonl-import://tiered-recall/filler/{index}",
            "superseded_by": None,
            "supersedes": None,
            "redacted": False,
            "redaction_reason": None,
        })
    write(handle, {
        "schema": "ee.export.memory.v1",
        "memory_id": "mem_tiered_recall_cold_required",
        "workspace_id": "ws-tiered-recall-source",
        "level": "procedural",
        "kind": "failure",
        "content": (
            "Tiered recall cold explicit failure evidence sentinel: keep required cold "
            "failure evidence eligible even when hot and warm tiers are full."
        ),
        "importance": 0.02,
        "confidence": 0.02,
        "utility": 0.02,
        "trust_class": "human_explicit",
        "trust_subclass": "tiered-recall-e2e",
        "created_at": created_at,
        "updated_at": created_at,
        "tombstoned_at": None,
        "tombstoned_reason": None,
        "valid_from": None,
        "valid_to": None,
        "expires_at": None,
        "source_agent": "tiered-recall-e2e",
        "provenance_uri": "jsonl-import://tiered-recall/cold-required",
        "superseded_by": None,
        "supersedes": None,
        "redacted": False,
        "redaction_reason": None,
    })
    write(handle, {
        "schema": "ee.export.footer.v1",
        "export_id": export_id,
        "completed_at": created_at,
        "total_records": filler_count + 3,
        "memory_count": filler_count + 1,
        "link_count": 0,
        "tag_count": 0,
        "audit_count": 0,
        "checksum": None,
        "success": True,
        "error_message": None,
    })
PY

IMPORT_JSON=$(ee_workspace import jsonl --source "$RECORDS_PATH" --json || true)
assert_jq "$IMPORT_JSON" '.success // false' "true" "tiered_recall_jsonl_import_success"
assert_jq "$IMPORT_JSON" '.data.memoriesImported // 0' "$CANDIDATE_POOL" "tiered_recall_jsonl_import_count"

INDEX_JSON=$(ee_workspace index rebuild --json || true)
assert_jq "$INDEX_JSON" '.success // false' "true" "tiered_recall_index_rebuild_success"

DEFAULT_JSON=$(ee_workspace context "$QUERY" --max-tokens 20000 --candidate-pool "$CANDIDATE_POOL" --json || true)
assert_jq "$DEFAULT_JSON" '.success // false' "true" "tiered_recall_default_context_success"
DEFAULT_HASH=$(hash_json_projection "$DEFAULT_JSON" '.data.pack')
write_pack_config false
DISABLED_JSON=$(ee_workspace context "$QUERY" --max-tokens 20000 --candidate-pool "$CANDIDATE_POOL" --json || true)
assert_jq "$DISABLED_JSON" '.success // false' "true" "tiered_recall_explicit_disabled_context_success"
DISABLED_HASH=$(hash_json_projection "$DISABLED_JSON" '.data.pack')
e2e_log_assert_eq "$DISABLED_HASH" "$DEFAULT_HASH" "tiered_recall_default_disabled_pack_hash_parity"

DISABLED_PERF=$(ee_workspace context "$QUERY" --max-tokens 20000 --candidate-pool "$CANDIDATE_POOL" --explain-performance --json || true)
assert_jq "$DISABLED_PERF" '.success // false' "true" "tiered_recall_disabled_perf_success"
DISABLED_TIER_BOOSTED=$(printf '%s' "$DISABLED_PERF" | jq -r '.data.candidates.tierBoostedCandidates // -1' 2>/dev/null || echo -1)
DISABLED_TIER_COLD=$(printf '%s' "$DISABLED_PERF" | jq -r '.data.candidates.tierColdCandidates // -1' 2>/dev/null || echo -1)
e2e_log_assert_eq "$DISABLED_TIER_BOOSTED/$DISABLED_TIER_COLD" "0/0" "tiered_recall_disabled_has_no_tier_effect"

write_pack_config true
ENABLED_JSON=$(ee_workspace context "$QUERY" --max-tokens 20000 --candidate-pool "$CANDIDATE_POOL" --json || true)
assert_jq "$ENABLED_JSON" '.success // false' "true" "tiered_recall_enabled_context_success"
ENABLED_HASH=$(hash_json_projection "$ENABLED_JSON" '.data.pack')
COLD_SELECTED=$(printf '%s' "$ENABLED_JSON" | jq -r '
    [.data.pack.items[]?
     | select((.content // "") | contains("cold explicit failure evidence sentinel"))
     | select((.why // "") | contains("tierAdmission tier=cold"))
     | select((.why // "") | contains("requiredEvidencePreserved=true"))]
    | length > 0
' 2>/dev/null || echo false)
e2e_log_assert_eq "$COLD_SELECTED" "true" "tiered_recall_required_cold_evidence_selected"

: >"$PERF_SAMPLES"
ENABLED_PERF=""
i=0
while [ "$i" -lt "$PERF_REPEATS" ]; do
    ENABLED_PERF=$(ee_workspace context "$QUERY" --max-tokens 20000 --candidate-pool "$CANDIDATE_POOL" --explain-performance --json || true)
    TOTAL_MS=$(printf '%s' "$ENABLED_PERF" | jq -r '[.data.timings[]? | select(.name == "total")][0].elapsedMs // empty' 2>/dev/null || true)
    [ -n "$TOTAL_MS" ] && printf '%s\n' "$TOTAL_MS" >>"$PERF_SAMPLES"
    i=$((i + 1))
done

assert_jq "$ENABLED_PERF" '.success // false' "true" "tiered_recall_enabled_perf_success"
TIER_BOOSTED=$(printf '%s' "$ENABLED_PERF" | jq -r '.data.candidates.tierBoostedCandidates // 0' 2>/dev/null || echo 0)
TIER_COLD=$(printf '%s' "$ENABLED_PERF" | jq -r '.data.candidates.tierColdCandidates // 0' 2>/dev/null || echo 0)
TIER_REQUIRED_COLD=$(printf '%s' "$ENABLED_PERF" | jq -r '.data.candidates.tierRequiredColdCandidates // 0' 2>/dev/null || echo 0)
TIMING_PRESENT=$(printf '%s' "$ENABLED_PERF" | jq -r '[.data.timings[]? | select(.name == "memoryTierAdmission")] | length > 0' 2>/dev/null || echo false)

e2e_log_assert_num "$TIER_BOOSTED" -ge 1 "tiered_recall_enabled_boosts_hot_warm_candidates"
e2e_log_assert_num "$TIER_COLD" -ge 1 "tiered_recall_enabled_reports_cold_candidates"
e2e_log_assert_num "$TIER_REQUIRED_COLD" -ge 1 "tiered_recall_enabled_reports_required_cold_candidates"
e2e_log_assert_eq "$TIMING_PRESENT" "true" "tiered_recall_memory_tier_timing_present"

PERCENTILES=$(python3 - "$PERF_SAMPLES" <<'PY'
import json
import sys

values = []
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    for line in handle:
        try:
            values.append(float(line.strip()))
        except ValueError:
            pass
values.sort()
if not values:
    print(json.dumps({"p50": None, "p95": None}))
else:
    def percentile(p):
        index = int(round((len(values) - 1) * p))
        return values[max(0, min(index, len(values) - 1))]
    print(json.dumps({"p50": percentile(0.50), "p95": percentile(0.95)}))
PY
)
P50_MS=$(printf '%s' "$PERCENTILES" | jq -r '.p50 // 0')
P95_MS=$(printf '%s' "$PERCENTILES" | jq -r '.p95 // 0')

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
PARITY="false"
[ "$DISABLED_HASH" = "$DEFAULT_HASH" ] && PARITY="true"
STATUS="passed"
[ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ] && STATUS="failed"
emit_tiered_recall_event "$STATUS" "$DEFAULT_HASH" "$DISABLED_HASH" "$ENABLED_HASH" "$PARITY" \
    "$TIER_BOOSTED" "$TIER_COLD" "$TIER_REQUIRED_COLD" "$P50_MS" "$P95_MS" \
    "$TIMING_PRESENT" "$COLD_SELECTED" "$ELAPSED_MS"

e2e_log_note "tiered_recall_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} p50_ms=${P50_MS} p95_ms=${P95_MS} tier_boosted=${TIER_BOOSTED} tier_cold=${TIER_COLD} tier_required_cold=${TIER_REQUIRED_COLD}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
