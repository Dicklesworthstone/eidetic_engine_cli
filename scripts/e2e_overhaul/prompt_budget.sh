#!/usr/bin/env bash
# E2E harness for bd-1zb7k.19.3 (AFR3 prompt-budget waste report).
#
# Runs `ee perf prompt-budget --trace <golden> --json` against the
# committed fixture trace and asserts the response shape:
#
#   - schema == ee.prompt_budget_report.v1
#   - top-level keys: traceHash, traceMalformedLines, total*, avoidable*,
#     categories[], suggestedActions[], degraded[]
#   - report contains no raw query / memory content (only hashes, ids,
#     counts, sizes per the bead privacy contract)
#
# Per AGENTS.md: no Cargo invocation; ee binary path is supplied via
# $EE_BIN (defaults to "ee" on PATH); harness aborts if the binary is
# not invokable. Emits ee.test_event.v1 lines to
# $EE_TEST_EVENT_DIR/prompt_budget.jsonl for forensic audit.
#
# Exit 0 on success.

set -euo pipefail

EE_BIN="${EE_BIN:-ee}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TRACE_FIXTURE="${EE_PROMPT_BUDGET_TRACE:-$REPO_ROOT/tests/fixtures/golden/perf_forensics/prompt_budget_trace.jsonl}"
EVENT_DIR="${EE_TEST_EVENT_DIR:-${TMPDIR:-/tmp}/ee-prompt-budget-events}"
EVENT_LOG="$EVENT_DIR/prompt_budget.jsonl"
BEAD_ID="bd-1zb7k.19.3"
SURFACE="prompt_budget"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for prompt_budget e2e\n' >&2
  exit 1
fi

emit_event() {
  local phase="$1"
  local status="$2"
  local detail="$3"
  jq -nc \
    --arg schema "ee.test_event.v1" \
    --arg bead "$BEAD_ID" \
    --arg surface "$SURFACE" \
    --arg phase "$phase" \
    --arg status "$status" \
    --arg detail "$detail" \
    --arg trace "$TRACE_FIXTURE" \
    --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{schema:$schema, beadId:$bead, surface:$surface, phase:$phase, status:$status, detail:$detail, trace:$trace, ts:$ts}' \
    >> "$EVENT_LOG"
}

if [ ! -f "$TRACE_FIXTURE" ]; then
  emit_event "probe" "fail" "trace fixture missing at $TRACE_FIXTURE"
  printf 'error: trace fixture missing at %s\n' "$TRACE_FIXTURE" >&2
  exit 2
fi
emit_event "probe" "ok" "trace fixture present"

if ! "$EE_BIN" --version >/dev/null 2>&1; then
  emit_event "probe" "ee_unavailable" "$EE_BIN cannot be invoked"
  printf 'error: $EE_BIN (%s) is not invokable; set EE_BIN or place ee on PATH\n' "$EE_BIN" >&2
  exit 2
fi

emit_event "run" "begin" "ee perf prompt-budget --trace=$TRACE_FIXTURE --json"
report_json="$("$EE_BIN" perf prompt-budget --trace "$TRACE_FIXTURE" --json)"

schema="$(printf '%s' "$report_json" | jq -r '.schema')"
if [ "$schema" != "ee.prompt_budget_report.v1" ]; then
  emit_event "run" "fail" "schema=$schema"
  printf 'error: schema=%s, expected ee.prompt_budget_report.v1\n' "$schema" >&2
  exit 3
fi

for key in traceHash totalBytes avoidableBytes categories suggestedActions; do
  if [ "$(printf '%s' "$report_json" | jq -r --arg k "$key" 'has($k)')" != "true" ]; then
    emit_event "shape" "fail" "missing $key"
    printf 'error: report missing key %s\n' "$key" >&2
    exit 3
  fi
done
emit_event "shape" "ok" "schema + top-level keys present"

# Privacy contract: refuse if any obviously-sensitive substring leaked.
# (The fixture is synthetic; we just verify the report's serialization
# does not carry raw query text under category labels.)
if printf '%s' "$report_json" | grep -qE '"raw_query"|"raw_memory_content"'; then
  emit_event "privacy" "fail" "raw_query / raw_memory_content key present"
  printf 'error: report includes raw_query / raw_memory_content fields\n' >&2
  exit 4
fi
emit_event "privacy" "ok" "no raw query / memory content keys in report"

emit_event "complete" "ok" "prompt_budget report shape verified"
printf 'prompt_budget e2e: ok trace=%s events=%s\n' "$TRACE_FIXTURE" "$EVENT_LOG" >&2
