#!/usr/bin/env bash
# e2e_diag_contention.sh — real-binary proof for the read-only contention
# diagnostic (ee.diag.contention.v1, bd-d67os Track D leaf 3 / bd-d67os.13).
#
# WHAT THIS PROVES (cross-process, through the real ee binary):
#   * Genuine contention is GENERATED: N concurrent `ee remember` writers stress
#     the per-workspace write lock while M concurrent identical `ee search`
#     readers exercise the read path / coalescing against ONE shared workspace.
#   * That contention is DETECTED as handled correctly: every write lands durably
#     exactly once (db_memory_count delta == N) and every record is searchable —
#     i.e. the write lock serialized the storm without losing/duplicating writes
#     or starving a reader.
#   * `ee diag contention --json` emits a well-formed, schema-valid
#     ee.diag.contention.v1 report (correct schemaTag, posture from the
#     {ok,warm,hot,contended} enum, a severity-ranked topContention array, and an
#     unavailableSources array) and is DETERMINISTIC across repeated one-shot
#     invocations.
#   * `ee diag contention --use-daemon` with no daemon running degrades
#     GRACEFULLY: it still exits 0, still emits a valid report (falling back to
#     the in-process snapshot), and records a degraded entry — the degraded-source
#     handling path.
#
# WHAT THIS DELIBERATELY DOES NOT ASSERT (and why — same honesty constraint as
# scripts/e2e_group_commit.sh):
#   A one-shot `ee diag contention` reads PROCESS-LOCAL telemetry: its own
#   (zeroed) group-commit atomics and an idle single-flight registry, and leaves
#   the write-owner queue / read-pool stats as unavailableSources because a CLI
#   invocation has no live actor/pool handle. The cross-process write-lock
#   contention generated above lives at the SQLite file-lock layer, which a fresh
#   diag process cannot observe. So a one-shot report's overallPosture is
#   expected to be "ok" with gaps — asserting posture!=ok here would be a fake
#   test. The live posture readout is the daemon path (`--use-daemon`, which reads
#   the daemon's accumulated coalescing) plus the in-process collector unit tests
#   (src/core/contention.rs) and the deterministic goldens
#   (tests/fixtures/golden/contention/*.json). This script pins the cross-process
#   CLI surface: real contention + a well-formed, deterministic, gracefully
#   degrading report. See bd-d67os.12 / bd-d67os.13 notes and ADR 0079.
#
# Emits ee.test_event.v1 JSON-line logs at each stage. NO mocks. RCH-only proof.
set -euo pipefail

TEST_ID="diag_contention_real_surface"
SCHEMA="ee.test_event.v1"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_ROOT="${EE_E2E_TMPDIR:-/private/tmp}/ee-e2e-diag-contention-$$"
WORKSPACE="$ARTIFACT_ROOT/workspace"
EVENT_LOG="$ARTIFACT_ROOT/diag_contention_events.jsonl"
PERF_SUMMARY="$ARTIFACT_ROOT/diag_contention_perf_summary.json"
WRITERS="${EE_DIAG_CONTENTION_E2E_WRITERS:-16}"
READERS="${EE_DIAG_CONTENTION_E2E_READERS:-12}"
SENTINEL="diag-contention-e2e-sentinel-$$"

mkdir -p "$WORKSPACE"
: >"$EVENT_LOG"

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

# Validate the structural contract of an ee.diag.contention.v1 envelope and emit
# a normalized (degraded-stripped) canonical form for determinism comparison.
validate_contention_report() {
  local json_path="${1:?json path required}"
  local canon_out="${2:?canonical output path required}"
  python3 - "$json_path" "$canon_out" <<'PY'
import json
import sys

path, canon_out = sys.argv[1], sys.argv[2]
with open(path, "r", encoding="utf-8") as handle:
    payload = json.load(handle)

errors = []
if payload.get("schema") != "ee.response.v2":
    errors.append(f"envelope schema={payload.get('schema')!r} (want ee.response.v2)")
data = payload.get("data", {})
if data.get("command") != "diag contention":
    errors.append(f"data.command={data.get('command')!r} (want 'diag contention')")
report = data.get("report", {})
if report.get("schemaTag") != "ee.diag.contention.v1":
    errors.append(f"report.schemaTag={report.get('schemaTag')!r}")
posture = report.get("overallPosture")
if posture not in ("ok", "warm", "hot", "contended"):
    errors.append(f"overallPosture={posture!r} not in posture enum")
top = report.get("topContention")
if not isinstance(top, list):
    errors.append("topContention is not an array")
else:
    rank = {"ok": 0, "warm": 1, "hot": 2, "contended": 3}
    prev = None
    for finding in top:
        for required in ("source", "severity", "reasonCode", "detail", "suggestedCommands"):
            if required not in finding:
                errors.append(f"finding missing {required}")
        sev = rank.get(finding.get("severity"), -1)
        if sev < 0:
            errors.append(f"finding severity={finding.get('severity')!r} not in enum")
        elif prev is not None and sev > prev:
            errors.append("topContention is not sorted severity-descending")
        else:
            prev = sev
gaps = report.get("unavailableSources")
if not isinstance(gaps, list):
    errors.append("unavailableSources is not an array")
else:
    for gap in gaps:
        if "source" not in gap or "code" not in gap:
            errors.append("gap missing source/code")
if not isinstance(payload.get("degraded"), list):
    errors.append("degraded is not an array")

if errors:
    print("INVALID: " + "; ".join(errors))
    sys.exit(1)

# Canonical form for the determinism check: the report body sans the (possibly
# environment-dependent) degraded annotations.
with open(canon_out, "w", encoding="utf-8") as handle:
    handle.write(json.dumps(report, sort_keys=True))
print(f"VALID posture={posture} findings={len(top)} gaps={len(gaps)}")
PY
}

assert_degraded_contains() {
  local json_path="${1:?json path required}"
  local code="${2:?code required}"
  python3 - "$json_path" "$code" <<'PY'
import json
import sys

path, code = sys.argv[1], sys.argv[2]
with open(path, "r", encoding="utf-8") as handle:
    payload = json.load(handle)
codes = [entry.get("code") for entry in payload.get("degraded", []) if isinstance(entry, dict)]
print("present" if code in codes else f"absent(codes={codes})")
PY
}

require_binary

emit_event "test_start" \
  "writers" "$WRITERS" \
  "readers" "$READERS" \
  "workspace" "$WORKSPACE" \
  "sentinel" "$SENTINEL" \
  "profile" "rch-no-mock-e2e"

run_ee_json "init_workspace" "$ARTIFACT_ROOT/init.json" init --workspace "$WORKSPACE" --json

run_ee_json "status_baseline" "$ARTIFACT_ROOT/status-baseline.json" \
  index status --workspace "$WORKSPACE" --json
baseline_count="$(extract_int_field "$ARTIFACT_ROOT/status-baseline.json" db_memory_count)"
if [[ "$baseline_count" -lt 0 ]]; then
  emit_event "assert_fail" \
    "label" "baseline_db_memory_count_readable" \
    "expected" "db_memory_count present in index status --json" \
    "actual" "field not found"
  exit 1
fi
emit_event "baseline_probe" "label" "db_memory_count_baseline" "actual" "$baseline_count"

# Seed one record so the concurrent readers have something to coalesce around.
run_ee_json "seed_record" "$ARTIFACT_ROOT/seed.json" \
  remember --workspace "$WORKSPACE" --level semantic --kind fact \
  "$SENTINEL seed record for read coalescing" --json
baseline_count=$((baseline_count + 1))

# ---- Genuine contention: N writers + M identical readers, one shared workspace.
emit_event "concurrency_phase_start" \
  "operation" "concurrent_write_read_storm" "writers" "$WRITERS" "readers" "$READERS"
storm_started="$(monotonic_ms)"

writer_pids=()
for i in $(seq 1 "$WRITERS"); do
  output="$ARTIFACT_ROOT/remember-$i.json"
  content="$SENTINEL concurrent contention record $i durable-under-load"
  (
    "$EE_BINARY" remember --workspace "$WORKSPACE" --level semantic --kind fact \
      "$content" --json >"$output" 2>"$output.stderr"
  ) &
  writer_pids+=("$!")
done

reader_pids=()
for j in $(seq 1 "$READERS"); do
  output="$ARTIFACT_ROOT/read-$j.json"
  # Identical query across all readers — this is the duplicate-read pressure that
  # a coalescing read path absorbs.
  (
    "$EE_BINARY" search "$SENTINEL seed" --workspace "$WORKSPACE" --limit 4 --json \
      >"$output" 2>"$output.stderr"
  ) &
  reader_pids+=("$!")
done

writer_failures=0
for idx in "${!writer_pids[@]}"; do
  if ! wait "${writer_pids[$idx]}"; then
    writer_failures=$((writer_failures + 1))
    n=$((idx + 1))
    emit_event "writer_failure" "writer" "$n" "stderr_path" "$ARTIFACT_ROOT/remember-$n.json.stderr"
  fi
done

reader_failures=0
for idx in "${!reader_pids[@]}"; do
  if ! wait "${reader_pids[$idx]}"; then
    reader_failures=$((reader_failures + 1))
    n=$((idx + 1))
    emit_event "reader_failure" "reader" "$n" "stderr_path" "$ARTIFACT_ROOT/read-$n.json.stderr"
  fi
done

storm_elapsed="$(elapsed_since "$storm_started")"
emit_event "concurrency_phase_end" \
  "operation" "concurrent_write_read_storm" \
  "writers" "$WRITERS" \
  "readers" "$READERS" \
  "writer_failures" "$writer_failures" \
  "reader_failures" "$reader_failures" \
  "elapsed_ms" "$storm_elapsed"

if [[ "$writer_failures" -ne 0 || "$reader_failures" -ne 0 ]]; then
  emit_event "assert_fail" \
    "label" "all_concurrent_clients_succeed" \
    "expected" "0 writer and 0 reader failures" \
    "actual" "writers=$writer_failures readers=$reader_failures"
  exit 1
fi
emit_event "assert_ok" "label" "all_concurrent_clients_succeed" \
  "actual" "writers=0/$WRITERS readers=0/$READERS"

# ---- Detection: every concurrent write landed durably exactly once. ----
run_ee_json "status_after_storm" "$ARTIFACT_ROOT/status.json" \
  index status --workspace "$WORKSPACE" --json
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
emit_event "assert_ok" "label" "all_writes_durable_exactly_once" "actual" "delta=$db_delta"

run_ee_json "search_sentinel" "$ARTIFACT_ROOT/search.json" \
  search "$SENTINEL" --workspace "$WORKSPACE" --limit "$((WRITERS * 2 + 4))" --json
hits="$(count_search_results "$ARTIFACT_ROOT/search.json")"
emit_event "retrieval_probe" "label" "sentinel_search_hits" "expected" ">=$WRITERS" "actual" "$hits"
if [[ "$hits" -lt "$WRITERS" ]]; then
  emit_event "assert_fail" \
    "label" "all_writes_indexed_under_concurrency" \
    "expected" "at least $WRITERS sentinel hits" \
    "actual" "$hits"
  exit 1
fi
emit_event "assert_ok" "label" "all_writes_indexed_under_concurrency" "actual" "hits=$hits"

# ---- The diagnostic surface: well-formed + deterministic one-shot report. ----
run_ee_json "diag_contention_a" "$ARTIFACT_ROOT/contention-a.json" \
  diag contention --workspace "$WORKSPACE" --json
run_ee_json "diag_contention_b" "$ARTIFACT_ROOT/contention-b.json" \
  diag contention --workspace "$WORKSPACE" --json

if summary_a="$(validate_contention_report "$ARTIFACT_ROOT/contention-a.json" "$ARTIFACT_ROOT/canon-a.json")"; then
  emit_event "assert_ok" "label" "contention_report_well_formed" "actual" "$summary_a"
else
  emit_event "assert_fail" \
    "label" "contention_report_well_formed" \
    "expected" "valid ee.diag.contention.v1 envelope" \
    "actual" "$summary_a"
  exit 1
fi

if summary_b="$(validate_contention_report "$ARTIFACT_ROOT/contention-b.json" "$ARTIFACT_ROOT/canon-b.json")"; then
  emit_event "assert_ok" "label" "contention_report_well_formed_repeat" "actual" "$summary_b"
else
  emit_event "assert_fail" \
    "label" "contention_report_well_formed_repeat" \
    "expected" "valid ee.diag.contention.v1 envelope" \
    "actual" "$summary_b"
  exit 1
fi

if diff -q "$ARTIFACT_ROOT/canon-a.json" "$ARTIFACT_ROOT/canon-b.json" >/dev/null 2>&1; then
  emit_event "assert_ok" "label" "contention_report_deterministic" \
    "actual" "two one-shot reports are byte-identical (sans degraded annotations)"
else
  emit_event "assert_fail" \
    "label" "contention_report_deterministic" \
    "expected" "identical report bodies across repeated one-shot invocations" \
    "actual" "canonical report bodies differ"
  exit 1
fi

# ---- Degraded-source handling: --use-daemon with no daemon falls back. ----
# No daemon is running in this temp workspace, so --use-daemon must record a
# degraded entry and still emit a valid in-process report (exit 0).
run_ee_json "diag_contention_use_daemon" "$ARTIFACT_ROOT/contention-daemon.json" \
  diag contention --workspace "$WORKSPACE" --use-daemon \
  --daemon-socket "$ARTIFACT_ROOT/no-such-daemon.sock" --json

if summary_d="$(validate_contention_report "$ARTIFACT_ROOT/contention-daemon.json" "$ARTIFACT_ROOT/canon-daemon.json")"; then
  emit_event "assert_ok" "label" "use_daemon_fallback_report_well_formed" "actual" "$summary_d"
else
  emit_event "assert_fail" \
    "label" "use_daemon_fallback_report_well_formed" \
    "expected" "valid report from in-process fallback" \
    "actual" "$summary_d"
  exit 1
fi

daemon_degraded="$(assert_degraded_contains "$ARTIFACT_ROOT/contention-daemon.json" "daemon_socket_unavailable")"
emit_event "degraded_probe" "label" "daemon_socket_unavailable_recorded" "actual" "$daemon_degraded"
if [[ "$daemon_degraded" != "present" ]]; then
  emit_event "assert_fail" \
    "label" "use_daemon_records_degraded_fallback" \
    "expected" "degraded entry with code daemon_socket_unavailable" \
    "actual" "$daemon_degraded"
  exit 1
fi
emit_event "assert_ok" "label" "use_daemon_records_degraded_fallback" "actual" "present"

write_perf_summary() {
  python3 - "$PERF_SUMMARY" "$TEST_ID" "$WRITERS" "$READERS" "$db_count" "$hits" "$storm_elapsed" "$EVENT_LOG" <<'PY'
import json
import sys

summary_path, test_id, writers, readers, db_count, hits, storm_elapsed, event_log = sys.argv[1:9]
summary = {
    "schema": "ee.perf.artifact_summary.v1",
    "artifactId": "diag-contention-real-surface-e2e",
    "artifactKind": "e2e_perf_probe",
    "sourceSchema": "ee.test_event.v1",
    "sourcePath": event_log,
    "contentHash": "computed-at-test-runtime",
    "observedHash": "computed-at-test-runtime",
    "profile": {"profileName": "rch-no-mock-e2e", "confidence": "medium"},
    "fixtureTier": "smoke",
    "commandFamily": "diag",
    "metrics": {
        "concurrent_writers": {"kind": "counted", "value": int(writers), "unit": "processes",
            "source": "concurrent ee-remember writers against one shared workspace"},
        "concurrent_readers": {"kind": "counted", "value": int(readers), "unit": "processes",
            "source": "concurrent identical ee-search readers against one shared workspace"},
        "durable_writes": {"kind": "counted", "value": int(db_count), "unit": "memories",
            "source": "ee index status db_memory_count after the storm"},
        "sentinel_search_hits": {"kind": "counted", "value": int(hits), "unit": "results",
            "source": "ee search sentinel after the storm"},
        "storm_wall_ms": {"kind": "measured", "value": float(storm_elapsed), "unit": "ms",
            "source": "wall time of the concurrent write+read storm"},
        "contention_report_cases": {"kind": "counted", "value": 3.0, "unit": "reports",
            "source": "two deterministic one-shot reports + one --use-daemon degraded fallback"},
    },
    "degraded": [],
    "redaction": "clean",
    "provenance": [
        {"field": "durable_writes", "sourcePath": event_log, "sourceLine": None},
        {"field": "contention_report_cases", "sourcePath": event_log, "sourceLine": None},
    ],
}
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
}

write_perf_summary

EXECUTION_SUBSTRATE="${EE_TEST_EXECUTION_SUBSTRATE:-local}"
if [[ -n "${RCH_WORKER_ID:-}${RCH_WORKER_HOST:-}" ]]; then
  EXECUTION_SUBSTRATE="rch"
fi
emit_event "artifact_manifest" \
  "manifest_schema" "ee.perf.artifact_summary.v1" \
  "phase" "diag_contention_e2e" \
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
  "fixture_filter" "diag_contention" \
  "log_path" "$EVENT_LOG" \
  "retention_manifest_path" "${EPIC_RETENTION_MANIFEST:-${EE_E2E_RETENTION_MANIFEST:-}}" \
  "artifact_manifest_hash" "computed-by-rch"

emit_event "test_pass" \
  "writers" "$WRITERS" \
  "readers" "$READERS" \
  "db_memory_count" "$db_count" \
  "sentinel_search_hits" "$hits" \
  "writer_failures" "0" \
  "reader_failures" "0"

echo "event_log=$EVENT_LOG"
echo "perf_summary=$PERF_SUMMARY"
