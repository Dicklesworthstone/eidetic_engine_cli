#!/usr/bin/env bash
# e2e_group_commit.sh — real-binary concurrency durability proof for the
# group-commit write hot path (bd-d67os Track B leaf 4).
#
# WHAT THIS PROVES (cross-process, observable through the real ee binary):
#   * N concurrent `ee remember` writers against ONE shared workspace all land
#     durably — no lost or duplicated writes under contention (db_memory_count
#     == N, sentinel search returns every record).
#   * The audit chain stays intact under concurrent load (`ee audit verify`).
#   * Every writer process exits 0 (no write-owner deadlock / lock starvation).
#
# WHAT THIS DELIBERATELY DOES NOT ASSERT (and why):
#   group-commit fsync_saved / batch-coalescing counters are PROCESS-LOCAL
#   atomics (src/core/write_owner.rs WRITE_GROUP_COMMIT_*). One-shot CLI writes
#   are single-writer fallbacks by design (run_one_shot_write_intake doc at
#   write_owner.rs:1339-1345), and `ee status` reads its own zeroed atomics, not
#   the daemon socket — so fsync_saved>0 is NOT observable across separate CLI
#   processes. That coalescing proof lives in the in-process proptest
#   (group_commit / write_group_commit suites, already green via RCH). Asserting
#   fsync_saved>0 here would be a fake test. See bd-d67os.4 / bd-d67os.12 notes.
#
# Emits ee.test_event.v1 JSON-line logs at each stage. NO mocks. RCH-only proof.
set -euo pipefail

TEST_ID="group_commit_concurrent_durability"
SCHEMA="ee.test_event.v1"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_ROOT="${EE_E2E_TMPDIR:-/private/tmp}/ee-e2e-group-commit-$$"
WORKSPACE="$ARTIFACT_ROOT/workspace"
EVENT_LOG="$ARTIFACT_ROOT/group_commit_events.jsonl"
LATENCY_LOG="$ARTIFACT_ROOT/remember_latency.tsv"
PERF_SUMMARY="$ARTIFACT_ROOT/group_commit_perf_summary.json"
WRITERS="${EE_GROUP_COMMIT_E2E_WRITERS:-24}"
SENTINEL="group-commit-e2e-sentinel-$$"

mkdir -p "$WORKSPACE"
: >"$EVENT_LOG"
: >"$LATENCY_LOG"

now_iso() {
  python3 - <<'PY'
from datetime import datetime, timezone
print(datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"))
PY
}

monotonic_ms() {
  python3 - <<'PY'
import time
print(time.monotonic_ns() / 1_000_000.0)
PY
}

emit_event() {
  local kind="${1:?kind required}"
  shift
  python3 - "$EVENT_LOG" "$SCHEMA" "$(now_iso)" "$TEST_ID" "$kind" "$@" <<'PY'
import json
import os
import sys

log_path, schema, ts, test_id, kind = sys.argv[1:6]
pairs = sys.argv[6:]
fields = {}
for i in range(0, len(pairs), 2):
    if i + 1 < len(pairs):
        fields[pairs[i]] = pairs[i + 1]
event = {
    "schema": schema,
    "ts": ts,
    "test_id": test_id,
    "kind": kind,
}
if fields:
    event["fields"] = fields
os.makedirs(os.path.dirname(log_path), exist_ok=True)
with open(log_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(event, sort_keys=True) + "\n")
PY
}

elapsed_since() {
  local started="${1:?started ms required}"
  python3 - "$started" "$(monotonic_ms)" <<'PY'
import sys
print(f"{float(sys.argv[2]) - float(sys.argv[1]):.3f}")
PY
}

require_binary() {
  if [[ -z "${EE_BINARY:-}" || ! -x "${EE_BINARY:-}" ]]; then
    emit_event "assert_fail" \
      "label" "ee_binary_required" \
      "expected" "EE_BINARY points to an executable ee binary" \
      "actual" "${EE_BINARY:-unset}"
    echo "EE_BINARY must point to a prebuilt executable ee binary" >&2
    exit 3
  fi
}

run_ee_json() {
  local label="${1:?label required}"
  local output="${2:?output path required}"
  shift 2
  local started elapsed status
  started="$(monotonic_ms)"
  if "$EE_BINARY" "$@" >"$output" 2>"$output.stderr"; then
    status="ok"
  else
    status="fail"
  fi
  elapsed="$(elapsed_since "$started")"
  emit_event "command_end" \
    "label" "$label" \
    "operation" "$1" \
    "status" "$status" \
    "elapsed_ms" "$elapsed" \
    "stdout_path" "$output" \
    "stderr_path" "$output.stderr"
  if [[ "$status" != "ok" ]]; then
    emit_event "assert_fail" \
      "label" "$label" \
      "expected" "exit status 0" \
      "actual" "nonzero exit"
    exit 1
  fi
}

# Tolerant extractor: find an integer field anywhere in the JSON envelope.
extract_int_field() {
  local json_path="${1:?json path required}"
  local field="${2:?field required}"
  python3 - "$json_path" "$field" <<'PY'
import json
import sys

path, field = sys.argv[1], sys.argv[2]
with open(path, "r", encoding="utf-8") as handle:
    payload = json.load(handle)

# Match the field regardless of serde casing: ee JSON envelopes are camelCase
# (e.g. dbMemoryCount), while callers pass the snake_case struct field name.
target = field.replace("_", "").lower()
found = []
def walk(node):
    if isinstance(node, dict):
        for key, value in node.items():
            if key.replace("_", "").lower() == target and isinstance(value, (int, float)):
                found.append(int(value))
            walk(value)
    elif isinstance(node, list):
        for item in node:
            walk(item)

walk(payload)
print(found[0] if found else -1)
PY
}

count_search_results() {
  local json_path="${1:?json path required}"
  python3 - "$json_path" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    payload = json.load(handle)

data = payload.get("data", {}) if isinstance(payload, dict) else {}
candidates = [
    data.get("results"),
    data.get("search", {}).get("results") if isinstance(data.get("search"), dict) else None,
    payload.get("results") if isinstance(payload, dict) else None,
]
for candidate in candidates:
    if isinstance(candidate, list):
        print(len(candidate))
        break
else:
    print(-1)
PY
}

require_binary

emit_event "test_start" \
  "writers" "$WRITERS" \
  "workspace" "$WORKSPACE" \
  "sentinel" "$SENTINEL" \
  "profile" "rch-no-mock-e2e"

run_ee_json "init_workspace" "$ARTIFACT_ROOT/init.json" init --workspace "$WORKSPACE" --json

# Baseline memory count BEFORE the storm — the durability assertion uses the
# delta so it stays correct even if `init` seeds bootstrap memories.
run_ee_json "status_baseline" "$ARTIFACT_ROOT/status-baseline.json" status --workspace "$WORKSPACE" --json
baseline_count="$(extract_int_field "$ARTIFACT_ROOT/status-baseline.json" db_memory_count)"
if [[ "$baseline_count" -lt 0 ]]; then
  emit_event "assert_fail" \
    "label" "baseline_db_memory_count_readable" \
    "expected" "db_memory_count present in status --json" \
    "actual" "field not found"
  exit 1
fi
emit_event "baseline_probe" "label" "db_memory_count_baseline" "actual" "$baseline_count"

# ---- Concurrent write storm: N writers race against ONE shared workspace. ----
emit_event "concurrency_phase_start" "operation" "concurrent_remember_storm" "writers" "$WRITERS"
storm_started="$(monotonic_ms)"
pids=()
for i in $(seq 1 "$WRITERS"); do
  output="$ARTIFACT_ROOT/remember-$i.json"
  content="$SENTINEL concurrent group-commit record $i durable-under-load"
  # Background each writer so they contend on the write-owner path simultaneously.
  (
    if "$EE_BINARY" remember --workspace "$WORKSPACE" --level semantic --kind fact \
        "$content" --json >"$output" 2>"$output.stderr"; then
      exit 0
    else
      exit 1
    fi
  ) &
  pids+=("$!")
done

# Collect every writer's exit code — any nonzero = lost write / lock starvation.
writer_failures=0
for idx in "${!pids[@]}"; do
  if ! wait "${pids[$idx]}"; then
    writer_failures=$((writer_failures + 1))
    n=$((idx + 1))
    emit_event "writer_failure" "writer" "$n" "stderr_path" "$ARTIFACT_ROOT/remember-$n.json.stderr"
  fi
done
storm_elapsed="$(elapsed_since "$storm_started")"
emit_event "concurrency_phase_end" \
  "operation" "concurrent_remember_storm" \
  "writers" "$WRITERS" \
  "writer_failures" "$writer_failures" \
  "elapsed_ms" "$storm_elapsed"

if [[ "$writer_failures" -ne 0 ]]; then
  emit_event "assert_fail" \
    "label" "all_concurrent_writers_succeed" \
    "expected" "0 writer failures" \
    "actual" "$writer_failures failed"
  exit 1
fi
emit_event "assert_ok" "label" "all_concurrent_writers_succeed" "actual" "0 failures of $WRITERS"

# ---- Durability: every concurrent write landed in the DB exactly once. ----
run_ee_json "status_after_storm" "$ARTIFACT_ROOT/status.json" status --workspace "$WORKSPACE" --json
db_count="$(extract_int_field "$ARTIFACT_ROOT/status.json" db_memory_count)"
db_delta=$((db_count - baseline_count))
emit_event "durability_probe" \
  "label" "db_memory_count_delta" \
  "expected" "$WRITERS" \
  "actual" "$db_delta" \
  "baseline" "$baseline_count" \
  "final" "$db_count"
if [[ "$db_delta" -ne "$WRITERS" ]]; then
  emit_event "assert_fail" \
    "label" "all_writes_durable_exactly_once" \
    "expected" "db_memory_count delta == $WRITERS" \
    "actual" "delta=$db_delta (baseline=$baseline_count final=$db_count)"
  exit 1
fi
emit_event "assert_ok" "label" "all_writes_durable_exactly_once" "actual" "delta=$db_delta final=$db_count"

# ---- Index/retrieval: every concurrent record is searchable by sentinel. ----
run_ee_json "search_sentinel" "$ARTIFACT_ROOT/search.json" \
  search "$SENTINEL" --workspace "$WORKSPACE" --limit "$((WRITERS * 2))" --json
hits="$(count_search_results "$ARTIFACT_ROOT/search.json")"
emit_event "retrieval_probe" "label" "sentinel_search_hits" "expected" "$WRITERS" "actual" "$hits"
if [[ "$hits" -ne "$WRITERS" ]]; then
  emit_event "assert_fail" \
    "label" "all_writes_indexed_under_concurrency" \
    "expected" "$WRITERS sentinel hits" \
    "actual" "$hits"
  exit 1
fi
emit_event "assert_ok" "label" "all_writes_indexed_under_concurrency" "actual" "hits=$hits"

# ---- Audit integrity: the audit chain survived the concurrent write storm. ----
if "$EE_BINARY" audit verify --workspace "$WORKSPACE" --json \
    >"$ARTIFACT_ROOT/audit-verify.json" 2>"$ARTIFACT_ROOT/audit-verify.json.stderr"; then
  emit_event "assert_ok" "label" "audit_chain_intact_under_concurrency" "actual" "audit verify exit 0"
else
  emit_event "assert_fail" \
    "label" "audit_chain_intact_under_concurrency" \
    "expected" "audit verify exit 0" \
    "actual" "nonzero exit (see audit-verify.json.stderr)"
  exit 1
fi

write_perf_summary() {
  python3 - "$PERF_SUMMARY" "$TEST_ID" "$WRITERS" "$db_count" "$hits" "$storm_elapsed" "$EVENT_LOG" <<'PY'
import json
import sys

summary_path, test_id, writers, db_count, hits, storm_elapsed, event_log = sys.argv[1:8]
summary = {
    "schema": "ee.perf.v1",
    "test_id": test_id,
    "phase": "group_commit_concurrent_durability",
    "writers": int(writers),
    "db_memory_count": int(db_count),
    "sentinel_search_hits": int(hits),
    "storm_wall_ms": float(storm_elapsed),
    "provenance": {
        "concurrent_durability_cases": {
            "value": int(writers),
            "source": "N concurrent ee-remember writers against one shared workspace, all landed exactly once",
        },
        "audit_integrity": {
            "value": "verified",
            "source": "ee audit verify after the concurrent write storm",
        },
    },
    "provenance_fields": [
        {"field": "concurrent_durability_cases", "sourcePath": event_log, "sourceLine": None},
        {"field": "audit_integrity", "sourcePath": event_log, "sourceLine": None},
    ],
}
with open(summary_path, "w", encoding="utf-8") as handle:
    handle.write(json.dumps(summary, indent=2, sort_keys=True) + "\n")
PY
}

write_perf_summary

EXECUTION_SUBSTRATE="${EE_TEST_EXECUTION_SUBSTRATE:-local}"
if [[ -n "${RCH_WORKER_ID:-}${RCH_WORKER_HOST:-}" ]]; then
  EXECUTION_SUBSTRATE="rch"
fi
emit_event "artifact_manifest" \
  "manifest_schema" "ee.perf.artifact_summary.v1" \
  "phase" "group_commit_e2e" \
  "binary_path" "$EE_BINARY" \
  "binary_hash" "computed-by-rch" \
  "binary_hash_status" "external" \
  "source_hash" "computed-by-rch" \
  "command_hash" "computed-by-rch" \
  "command_arg_count" "0" \
  "execution_substrate" "$EXECUTION_SUBSTRATE" \
  "local_host" "$(hostname 2>/dev/null || printf unknown)" \
  "worker_host" "${RCH_WORKER_HOST:-${RCH_WORKER_ID:-}}" \
  "target_directory" "${CARGO_TARGET_DIR:-}" \
  "fixture_filter" "group_commit" \
  "log_path" "$EVENT_LOG" \
  "retention_manifest_path" "${EPIC_RETENTION_MANIFEST:-${EE_E2E_RETENTION_MANIFEST:-}}" \
  "artifact_manifest_hash" "computed-by-rch"

emit_event "test_pass" \
  "writers" "$WRITERS" \
  "db_memory_count" "$db_count" \
  "sentinel_search_hits" "$hits" \
  "writer_failures" "0"

echo "event_log=$EVENT_LOG"
echo "perf_summary=$PERF_SUMMARY"
