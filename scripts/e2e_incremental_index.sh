#!/usr/bin/env bash
set -euo pipefail

TEST_ID="incremental_index_flat_latency"
SCHEMA="ee.test_event.v1"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_ROOT="${EE_E2E_TMPDIR:-/private/tmp}/ee-e2e-incremental-index-$$"
WORKSPACE="$ARTIFACT_ROOT/workspace"
EVENT_LOG="$ARTIFACT_ROOT/incremental_index_events.jsonl"
LATENCY_LOG="$ARTIFACT_ROOT/remember_latency.tsv"
PERF_SUMMARY="$ARTIFACT_ROOT/incremental_index_perf_summary.json"
CORPUS_SIZE="${EE_INCREMENTAL_E2E_CORPUS_SIZE:-16}"
SEARCH_QUERY="${EE_INCREMENTAL_E2E_QUERY:-incremental index e2e sentinel}"

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

compare_search_results() {
  local before="${1:?before search json required}"
  local after="${2:?after search json required}"
  python3 - "$before" "$after" <<'PY'
import json
import sys

def load(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)

def result_ids(payload):
    data = payload.get("data", {})
    candidates = [
        data.get("results"),
        data.get("search", {}).get("results") if isinstance(data.get("search"), dict) else None,
        payload.get("results"),
    ]
    for candidate in candidates:
        if isinstance(candidate, list):
            ids = []
            for item in candidate:
                if isinstance(item, dict):
                    ids.append(
                        item.get("docId")
                        or item.get("memoryId")
                        or item.get("memory_id")
                        or item.get("id")
                        or ""
                    )
            return ids
    return []

before_ids = result_ids(load(sys.argv[1]))
after_ids = result_ids(load(sys.argv[2]))
if not before_ids:
    print("incremental search returned no result ids")
    sys.exit(1)
if before_ids != after_ids:
    print(f"incremental={before_ids} rebuild={after_ids}")
    sys.exit(1)
print("match")
PY
}

assert_flat_latency() {
  python3 - "$LATENCY_LOG" "${EE_INCREMENTAL_E2E_MAX_LATENCY_RATIO:-6.0}" "${EE_INCREMENTAL_E2E_LATENCY_SLACK_MS:-250.0}" <<'PY'
import statistics
import sys

path, ratio_raw, slack_raw = sys.argv[1:4]
latencies = []
with open(path, "r", encoding="utf-8") as handle:
    for line in handle:
        parts = line.strip().split("\t")
        if len(parts) == 2:
            latencies.append(float(parts[1]))
if len(latencies) < 6:
    print("too few latency samples")
    sys.exit(1)
first = statistics.mean(latencies[:3])
last = statistics.mean(latencies[-3:])
ratio = last / max(first, 1.0)
allowed = float(ratio_raw)
slack = float(slack_raw)
if last > first * allowed + slack:
    print(f"first_avg_ms={first:.3f} last_avg_ms={last:.3f} ratio={ratio:.3f}")
    sys.exit(1)
print(f"first_avg_ms={first:.3f} last_avg_ms={last:.3f} ratio={ratio:.3f}")
PY
}

write_perf_summary() {
  python3 - "$EVENT_LOG" "$LATENCY_LOG" "$PERF_SUMMARY" "$CORPUS_SIZE" <<'PY'
import json
import statistics
import sys

event_log, latency_log, output, corpus_size = sys.argv[1:5]
latencies = []
with open(latency_log, "r", encoding="utf-8") as handle:
    for line in handle:
        parts = line.strip().split("\t")
        if len(parts) == 2:
            latencies.append(float(parts[1]))
full_rebuild_elapsed = 0.0
with open(event_log, "r", encoding="utf-8") as handle:
    for line in handle:
        event = json.loads(line)
        fields = event.get("fields", {})
        if event.get("kind") == "command_end" and fields.get("label") == "full_rebuild_baseline":
            full_rebuild_elapsed = float(fields.get("elapsed_ms", 0.0))
first = statistics.mean(latencies[:3]) if len(latencies) >= 3 else 0.0
last = statistics.mean(latencies[-3:]) if len(latencies) >= 3 else 0.0
ratio = last / max(first, 1.0) if latencies else 0.0
payload = {
    "schema": "ee.perf.artifact_summary.v1",
    "artifactId": "incremental-index-intake-e2e",
    "artifactKind": "e2e_perf_probe",
    "sourceSchema": "ee.test_event.v1",
    "sourcePath": event_log,
    "contentHash": "computed-at-test-runtime",
    "observedHash": "computed-at-test-runtime",
    "profile": {"profileName": "rch-no-mock-e2e", "confidence": "medium"},
    "fixtureTier": "smoke",
    "commandFamily": "remember",
    "metrics": {
        "incremental_first_window_avg_ms": {
            "kind": "measured",
            "value": round(first, 3),
            "unit": "ms",
            "source": "ee.test_event.v1 bench_iteration elapsed_ms",
        },
        "incremental_last_window_avg_ms": {
            "kind": "measured",
            "value": round(last, 3),
            "unit": "ms",
            "source": "ee.test_event.v1 bench_iteration elapsed_ms",
        },
        "flat_latency_ratio": {
            "kind": "derived",
            "value": round(ratio, 6),
            "unit": "ratio",
            "source": "last_window_avg_ms / first_window_avg_ms",
        },
        "full_rebuild_baseline_elapsed_ms": {
            "kind": "measured",
            "value": round(full_rebuild_elapsed, 3),
            "unit": "ms",
            "source": "ee.test_event.v1 command_end elapsed_ms for full_rebuild_baseline",
        },
        "rebuild_avoided_count": {
            "kind": "counted",
            "value": float(max(int(corpus_size) - 1, 0)),
            "unit": "writes",
            "source": "incremental remember writes before full-rebuild baseline",
        },
        "search_equivalence_cases": {
            "kind": "counted",
            "value": 1.0,
            "unit": "comparisons",
            "source": "incremental search result ids compared with post-rebuild result ids",
        },
    },
    "degraded": [],
    "redaction": "clean",
    "provenance": [
        {"field": "elapsed_ms", "sourcePath": event_log, "sourceLine": None},
        {"field": "search_equivalence_cases", "sourcePath": event_log, "sourceLine": None},
    ],
}
with open(output, "w", encoding="utf-8") as handle:
    json.dump(payload, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
}

require_binary
emit_event "note" "label" "artifact_root" "path" "$ARTIFACT_ROOT"
run_ee_json "init_workspace" "$ARTIFACT_ROOT/init.json" init --workspace "$WORKSPACE" --json

for i in $(seq 1 "$CORPUS_SIZE"); do
  output="$ARTIFACT_ROOT/remember-$i.json"
  content="incremental index e2e sentinel record $i common-token release notes"
  started="$(monotonic_ms)"
  run_ee_json "remember_$i" "$output" remember --workspace "$WORKSPACE" --level semantic --kind fact "$content" --json
  elapsed="$(elapsed_since "$started")"
  printf '%s\t%s\n' "$i" "$elapsed" >>"$LATENCY_LOG"
  emit_event "bench_iteration" \
    "operation" "remember_incremental_index_write" \
    "status" "ok" \
    "profile" "rch-no-mock-e2e" \
    "workload_tier" "growing_corpus" \
    "corpus_size" "$i" \
    "elapsed_ms" "$elapsed"
done

run_ee_json "search_before_full_rebuild" "$ARTIFACT_ROOT/search-before.json" \
  search "$SEARCH_QUERY" --workspace "$WORKSPACE" --limit 8 --json
run_ee_json "full_rebuild_baseline" "$ARTIFACT_ROOT/index-rebuild.json" \
  index rebuild --workspace "$WORKSPACE" --json
run_ee_json "search_after_full_rebuild" "$ARTIFACT_ROOT/search-after.json" \
  search "$SEARCH_QUERY" --workspace "$WORKSPACE" --limit 8 --json

if comparison="$(compare_search_results "$ARTIFACT_ROOT/search-before.json" "$ARTIFACT_ROOT/search-after.json")"; then
  emit_event "assert_ok" "label" "incremental_search_matches_full_rebuild" "actual" "$comparison"
else
  emit_event "assert_fail" \
    "label" "incremental_search_matches_full_rebuild" \
    "expected" "same result ids and ordering" \
    "actual" "$comparison"
  exit 1
fi

if flat="$(assert_flat_latency)"; then
  emit_event "assert_ok" "label" "incremental_write_latency_flat" "actual" "$flat"
else
  emit_event "assert_fail" \
    "label" "incremental_write_latency_flat" \
    "expected" "last latency window within configured ratio and slack" \
    "actual" "$flat"
  exit 1
fi

write_perf_summary
EXECUTION_SUBSTRATE="${EE_TEST_EXECUTION_SUBSTRATE:-local}"
if [[ -n "${RCH_WORKER_ID:-}${RCH_WORKER_HOST:-}" ]]; then
  EXECUTION_SUBSTRATE="rch"
fi
emit_event "artifact_manifest" \
  "manifest_schema" "ee.perf.artifact_summary.v1" \
  "phase" "incremental_index_e2e" \
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
  "fixture_filter" "incremental_index" \
  "log_path" "$EVENT_LOG" \
  "retention_manifest_path" "${EPIC_RETENTION_MANIFEST:-${EE_E2E_RETENTION_MANIFEST:-}}" \
  "artifact_manifest_hash" "computed-by-rch"

echo "event_log=$EVENT_LOG"
echo "perf_summary=$PERF_SUMMARY"
