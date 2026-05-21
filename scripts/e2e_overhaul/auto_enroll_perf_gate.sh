#!/usr/bin/env bash
set -euo pipefail

# SRR6.46.15 auto-enroll performance gate contract.
#
# This script is read-only. It validates the checked-in baseline, bench profile
# plumbing, and optional candidate report comparison. Cargo/Rust execution must
# happen through RCH; this script only emits the RCH command a verifier should
# use when a measured report is needed.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

BASELINE="${EE_AUTO_ENROLL_PERF_BASELINE:-benches/baselines/auto_enroll_perf_v0.json}"
CANDIDATE_REPORT="${EE_AUTO_ENROLL_PERF_REPORT:-}"

emit_event() {
  local phase="$1"
  local status="$2"
  local detail="$3"
  printf '{"schema":"ee.test_event.v1","surface":"auto_enroll_perf_gate","phase":"%s","status":"%s","detail":"%s"}\n' \
    "$phase" "$status" "$detail"
}

fail() {
  emit_event "$1" "fail" "$2"
  exit 1
}

require_file() {
  local path="$1"
  test -f "$path" || fail "setup" "missing file: $path"
  emit_event "setup" "pass" "file exists: $path"
}

require_text() {
  local path="$1"
  local text="$2"
  grep -Fq -- "$text" "$path" || fail "source_contract" "missing text in $path: $text"
}

command -v jq >/dev/null 2>&1 || fail "setup" "jq is required"

require_file "$BASELINE"
require_file "scripts/bench.sh"
require_file "scripts/bench_perf_regression.sh"
require_file "tests/auto_enroll_perf_baseline.rs"

jq -e '.schema == "ee.perf.baseline.v1"' "$BASELINE" >/dev/null \
  || fail "baseline" "baseline schema mismatch"
jq -e '.sourceBead == "bd-36bbk.1.15"' "$BASELINE" >/dev/null \
  || fail "baseline" "baseline sourceBead mismatch"
jq -e '.hardware_class == "mac-m3-pro"' "$BASELINE" >/dev/null \
  || fail "baseline" "baseline hardware class mismatch"
jq -e '.regression_margin.active_p99_pct == 15' "$BASELINE" >/dev/null \
  || fail "baseline" "active p99 margin must be 15 percent"
jq -e '.regression_margin.idle_p99_pct == 20' "$BASELINE" >/dev/null \
  || fail "baseline" "idle p99 margin must be 20 percent"
jq -e '.regression_margin.idle_rss_slope_mb_per_hour_max == 0.7' "$BASELINE" >/dev/null \
  || fail "baseline" "idle 24h RSS slope ceiling must be 0.7 MB/h"
jq -e '.active_workload_rows | length == 15' "$BASELINE" >/dev/null \
  || fail "baseline" "expected 15 active workload rows"
jq -e '.idle_workload_rows | length == 4' "$BASELINE" >/dev/null \
  || fail "baseline" "expected 4 idle workload rows"
jq -e '.scale_workload_rows | length == 2' "$BASELINE" >/dev/null \
  || fail "baseline" "expected 2 scale workload rows"
jq -e '
  [.operations | to_entries[] | select((.value.p50_ms | type) != "number" or (.value.p99_ms | type) != "number")]
  | length == 0
' "$BASELINE" >/dev/null || fail "baseline" "every operation must define numeric p50_ms and p99_ms"
emit_event "baseline" "pass" "auto-enroll baseline shape is valid"

require_text "scripts/bench.sh" "auto_enroll"
require_text "scripts/bench.sh" "auto_enroll_idle_24h"
require_text "scripts/bench.sh" "append_auto_enroll_baseline_rows"
require_text "README.md" "auto_enroll_perf_v0.json"
emit_event "source_contract" "pass" "bench profile and README references present"

if [ -n "$CANDIDATE_REPORT" ]; then
  require_file "$CANDIDATE_REPORT"
  jq -e --slurpfile baseline "$BASELINE" '
    .operations as $candidate_ops
    | $baseline[0].operations
    | to_entries
    | map(
        . as $base
        | ($candidate_ops[$base.key].p99_ms // null) as $candidate_p99
        | ($base.value.tolerance_pct_p99 // 15) as $margin
        | select($candidate_p99 != null)
        | select($candidate_p99 > ($base.value.p99_ms * (1 + ($margin / 100))))
      )
    | length == 0
  ' "$CANDIDATE_REPORT" >/dev/null || fail "candidate_report" "candidate report exceeds baseline p99 margins"
  emit_event "candidate_report" "pass" "candidate report stays within baseline margins"
else
  emit_event "candidate_report" "info" "no EE_AUTO_ENROLL_PERF_REPORT supplied; static baseline contract only"
fi

emit_event "rch_command" "info" "rch exec -- env TMPDIR=/tmp CARGO_TARGET_DIR=/Volumes/USBNVME16TB/temp_agent_space/cargo-target cargo test --test auto_enroll_perf_baseline"
emit_event "complete" "pass" "auto-enroll performance gate contract is valid"
