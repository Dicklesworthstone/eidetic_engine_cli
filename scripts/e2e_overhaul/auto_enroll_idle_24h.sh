#!/usr/bin/env bash
set -euo pipefail

# SRR6.46.15 nightly-only idle daemon resource budget gate.
#
# The long-running measurement is intentionally opt-in. Without
# EE_E2E_NIGHTLY=1 this exits 78, matching the existing opt-in e2e convention.
# When enabled, provide EE_AUTO_ENROLL_IDLE_REPORT with a measured ee.perf.v1
# report from the RCH-run daemon idle sampler; this script compares the report
# against the checked-in 24h RSS and 1h CPU/FD budgets.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

BASELINE="${EE_AUTO_ENROLL_PERF_BASELINE:-benches/baselines/auto_enroll_perf_v0.json}"
IDLE_REPORT="${EE_AUTO_ENROLL_IDLE_REPORT:-}"

emit_event() {
  local phase="$1"
  local status="$2"
  local detail="$3"
  printf '{"schema":"ee.test_event.v1","surface":"auto_enroll_idle_24h","phase":"%s","status":"%s","detail":"%s"}\n' \
    "$phase" "$status" "$detail"
}

if [ "${EE_E2E_NIGHTLY:-0}" != "1" ]; then
  emit_event "setup" "skip" "set EE_E2E_NIGHTLY=1 and EE_AUTO_ENROLL_IDLE_REPORT=<report> to run the 24h idle gate"
  exit 78
fi

fail() {
  emit_event "$1" "fail" "$2"
  exit 1
}

command -v jq >/dev/null 2>&1 || fail "setup" "jq is required"
test -f "$BASELINE" || fail "setup" "missing baseline: $BASELINE"
test -n "$IDLE_REPORT" || fail "setup" "EE_AUTO_ENROLL_IDLE_REPORT is required for nightly idle gate"
test -f "$IDLE_REPORT" || fail "setup" "missing idle report: $IDLE_REPORT"

jq -e '.operations.ee_daemon_idle_rss_24h.rss_slope_mb_per_hour_max == 0.7' "$BASELINE" >/dev/null \
  || fail "baseline" "24h RSS slope ceiling drifted"
jq -e --slurpfile baseline "$BASELINE" '
  .operations as $ops
  | ($baseline[0].operations.ee_daemon_idle_rss_24h.p99_ms * 1.20) as $rss24h_p99_ceiling
  | ($baseline[0].operations.ee_daemon_idle_rss_24h.rss_slope_mb_per_hour_max) as $slope_ceiling
  | ($baseline[0].operations.ee_daemon_idle_cpu_1h.p99_ms * 1.20) as $cpu_p99_ceiling
  | ($baseline[0].operations.ee_daemon_idle_fd_count_1h.p99_ms * 1.20) as $fd_p99_ceiling
  | (($ops.ee_daemon_idle_rss_24h.p99_ms // 0) <= $rss24h_p99_ceiling)
    and (($ops.ee_daemon_idle_rss_24h.rss_slope_mb_per_hour // 0) <= $slope_ceiling)
    and (($ops.ee_daemon_idle_cpu_1h.p99_ms // 0) <= $cpu_p99_ceiling)
    and (($ops.ee_daemon_idle_fd_count_1h.p99_ms // 0) <= $fd_p99_ceiling)
' "$IDLE_REPORT" >/dev/null || fail "idle_report" "idle report exceeds 24h RSS, CPU, FD, or slope budget"

emit_event "idle_report" "pass" "idle report stays within 24h resource budgets"
emit_event "complete" "pass" "auto-enroll idle 24h gate is valid"
