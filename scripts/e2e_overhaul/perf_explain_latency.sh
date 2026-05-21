#!/usr/bin/env bash
# O1 — perf explain-latency e2e driver.
#
# This is a no-build harness. It uses an already-built ee binary, captures real
# search/context command_end events through the J1 logger, then verifies that
# ee perf explain-latency can attribute stages from that evidence.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq

if [ -z "${EE_TEST_LOG_PATH:-}" ]; then
    export EE_TEST_LOG_PATH="${TMPDIR:-/tmp}/ee-perf-explain-latency-${BASHPID:-$$}.jsonl"
fi
: >"$EE_TEST_LOG_PATH"

epic_setup "perf_explain_latency"

ARTIFACT_DIR="$EPIC_WORKSPACE/perf-explain-latency"
GOLDEN_DIR="$REPO_ROOT/tests/fixtures/golden/perf_forensics"
mkdir -p "$ARTIFACT_DIR"

normalize_latency_report() {
    jq '
      .data.report.queryShapeHash = "<J1_LOG_HASH>"
      | .data.report.stageTimings |= map(.elapsedMs = "<MEASURED>")
    '
}

assert_stage_present() {
    local json="$1"
    local stage="$2"
    local label="$3"
    local present
    present="$(printf '%s' "$json" | jq -r --arg stage "$stage" \
        'any(.data.report.stageTimings[]?; .stage == $stage)')"
    e2e_log_assert_eq "$present" "true" "$label"
}

MEMORY_JSON=$(ee_workspace remember \
    "Latency fixture memory for retrieval and pack timing." \
    --level procedural --kind rule --json)
assert_jq "$MEMORY_JSON" '.success' "true" "perf_latency_seed_memory_success"

INDEX_JSON=$(ee_workspace index rebuild --json)
assert_jq "$INDEX_JSON" '.success' "true" "perf_latency_index_rebuild_success"

SEARCH_JSON=$(ee_workspace search "latency retrieval pack timing" --json)
assert_jq "$SEARCH_JSON" '.success' "true" "perf_latency_search_success"

CONTEXT_JSON=$(ee_workspace context "latency retrieval pack timing" --max-tokens 900 --json)
assert_jq "$CONTEXT_JSON" '.success' "true" "perf_latency_context_success"

SEARCH_REPORT=$(ee_global perf explain-latency --surface search --log "$EE_TEST_LOG_PATH" --json)
assert_jq "$SEARCH_REPORT" '.success' "true" "perf_latency_search_report_success"
assert_jq "$SEARCH_REPORT" '.data.schema' "ee.perf.explain_latency.v1" \
    "perf_latency_search_report_schema"
assert_jq "$SEARCH_REPORT" '.data.effect.defaultEffect' "read_only" \
    "perf_latency_search_effect_read_only"
assert_jq "$SEARCH_REPORT" '.data.report.surface' "search" \
    "perf_latency_search_surface"
assert_stage_present "$SEARCH_REPORT" "retrieval" "perf_latency_search_retrieval_stage"
assert_jq "$SEARCH_REPORT" '.data.report.degraded | length' "0" \
    "perf_latency_search_no_degraded"

SEARCH_NORMALIZED="$ARTIFACT_DIR/explain_latency_search.normalized.json"
printf '%s' "$SEARCH_REPORT" | normalize_latency_report >"$SEARCH_NORMALIZED"
e2e_log_golden_compare "$SEARCH_NORMALIZED" \
    "$GOLDEN_DIR/explain_latency_search.json" \
    "perf_latency_search_golden"

CONTEXT_REPORT=$(ee_global perf explain-latency --surface context --log "$EE_TEST_LOG_PATH" --json)
assert_jq "$CONTEXT_REPORT" '.success' "true" "perf_latency_context_report_success"
assert_jq "$CONTEXT_REPORT" '.data.schema' "ee.perf.explain_latency.v1" \
    "perf_latency_context_report_schema"
assert_jq "$CONTEXT_REPORT" '.data.effect.defaultEffect' "read_only" \
    "perf_latency_context_effect_read_only"
assert_jq "$CONTEXT_REPORT" '.data.report.surface' "context" \
    "perf_latency_context_surface"
assert_stage_present "$CONTEXT_REPORT" "pack_selection" "perf_latency_context_pack_stage"
assert_jq "$CONTEXT_REPORT" '.data.report.degraded | length' "0" \
    "perf_latency_context_no_degraded"

CONTEXT_NORMALIZED="$ARTIFACT_DIR/explain_latency_context.normalized.json"
printf '%s' "$CONTEXT_REPORT" | normalize_latency_report >"$CONTEXT_NORMALIZED"
e2e_log_golden_compare "$CONTEXT_NORMALIZED" \
    "$GOLDEN_DIR/explain_latency_context.json" \
    "perf_latency_context_golden"

MISSING_REPORT="$ARTIFACT_DIR/missing_timing_artifact.json"
cat >"$MISSING_REPORT" <<'JSON'
{"schema":"ee.perf.artifact_summary.v1","artifactId":"latency-missing","artifactKind":"cache_report","sourceSchema":"ee.perf.source.v1","commandFamily":"search","metrics":{},"degraded":[],"redaction":"clean","provenance":[]}
JSON

MISSING_OUTPUT=$(ee_global perf explain-latency --surface search --report "$MISSING_REPORT" --json)
assert_jq "$MISSING_OUTPUT" '.success' "true" "perf_latency_missing_report_success"
assert_jq "$MISSING_OUTPUT" '.data.report.degraded[0].code' \
    "perf_latency_evidence_missing" "perf_latency_missing_degraded_code"
assert_jq "$MISSING_OUTPUT" '.data.report.stageTimings | length' "0" \
    "perf_latency_missing_no_fake_stage"

if [ "$EE_TEST_LOG_ASSERTS_FAIL" -ne 0 ]; then
    exit 1
fi
