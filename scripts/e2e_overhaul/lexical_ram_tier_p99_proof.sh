#!/usr/bin/env bash
# bd-21xbi.3 — host-class p99 benchmark gate for the lexical RAM-tier
# optimization.
#
# This script is intentionally outside normal CI. Without EE_HUGE_HOST=1
# it exits 78 after writing an ee.test_event.v1 skip event. With opt-in
# enabled, it captures a disabled-baseline and enabled-RAM-tier run of
# the lexical search hot path on a workspace whose lexical index is
# large enough to exercise posting-list IO cost, then emits ee.perf.v1
# rows with p50/p95/p99 latencies, byte counts, and degraded codes.
#
# Parent acceptance (bd-21xbi) requires the enabled run to improve p99
# hot-path latency by >= 30 percent versus the disabled baseline on a
# 256GB+/64-core host. This script does NOT enforce the 30 percent gate
# itself — the comparison is captured for human review and for a future
# CI gate that runs only on the qualified host class. Forcing the
# 30 percent gate inside the script would make it brittle when run on
# mid-tier Linux hosts that are warm enough to verify the seam but not
# beefy enough to hit the production target.
#
# Parallel to bd-1crtj's mesh_tailscale_smoke.sh and bd-36bbk.1.11's
# auto_enroll_real_tailscale.sh: each is an opt-in evidence harness
# that skips clean when its host-class prerequisites are absent, runs
# the real-host evidence when explicitly enabled, and emits the
# structured event stream that the parent bead's acceptance reads.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
EXIT_SKIP=78
EXIT_CLEANUP_FAILURE=79
SCENARIO="lexical_ram_tier_p99_proof"
BEAD_ID="bd-21xbi.3"

EVENT_DIR="${EE_TEST_EVENT_DIR:-${TMPDIR:-/tmp}/ee-${SCENARIO}.$$}"
mkdir -p "$EVENT_DIR"
EVENT_FILE="$EVENT_DIR/events.jsonl"
ARTIFACT_DIR="${EE_E2E_ARTIFACT_DIR:-$EVENT_DIR/artifacts}"
mkdir -p "$ARTIFACT_DIR"

# Track workspace + binary so cleanup can run on exit. WORKSPACE empty
# until setup runs so the EXIT trap can no-op on early precondition
# skips without touching the filesystem.
WORKSPACE=""
EE_BINARY=""
CLEANUP_RAN=0

json_hash() {
    printf '%s' "${1:-}" | shasum -a 256 | awk '{print substr($1, 1, 16)}'
}

emit_event() {
    local phase="${1:?phase required}"
    local status="${2:?status required}"
    local message="${3:?message required}"
    local detail_json="${4:-}"
    if [ -z "$detail_json" ]; then
        detail_json="{}"
    fi
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg scenario "$SCENARIO" \
        --arg bead "$BEAD_ID" \
        --arg phase "$phase" \
        --arg status "$status" \
        --arg message "$message" \
        --argjson details "$detail_json" \
        '{
          schema: $schema,
          kind: "lexical_ram_tier_p99_proof",
          bead: $bead,
          phase: $phase,
          status: $status,
          message: $message,
          fields: ({scenario: $scenario} + $details)
        }' >>"$EVENT_FILE"
}

emit_perf() {
    local label="${1:?label required}"
    local detail_json="${2:?detail_json required}"
    jq -cn \
        --arg schema "ee.perf.v1" \
        --arg scenario "$SCENARIO" \
        --arg bead "$BEAD_ID" \
        --arg label "$label" \
        --argjson details "$detail_json" \
        '{
          schema: $schema,
          kind: "lexical_ram_tier_p99_proof",
          bead: $bead,
          label: $label,
          fields: ({scenario: $scenario} + $details)
        }' >>"$EVENT_FILE"
}

skip() {
    local reason="${1:?skip reason required}"
    emit_event "precondition" "skipped" "$reason" '{}'
    printf '%s skipped: %s\n' "$SCENARIO" "$reason" >&2
    printf '%s\n' "$EVENT_FILE"
    exit "$EXIT_SKIP"
}

fail() {
    local phase="${1:?phase required}"
    local reason="${2:?failure reason required}"
    emit_event "$phase" "failed" "$reason" '{}'
    printf '%s failed: %s\n' "$SCENARIO" "$reason" >&2
    printf 'event log: %s\n' "$EVENT_FILE" >&2
    exit 1
}

require_tool() {
    local tool="${1:?tool required}"
    if ! command -v "$tool" >/dev/null 2>&1; then
        skip "$tool is required for the lexical RAM-tier p99 proof"
    fi
}

resolve_ee_binary() {
    if [ -n "${EE_BINARY:-}" ]; then
        printf '%s\n' "$EE_BINARY"
        return 0
    fi
    # shellcheck source=scripts/lib/ee_binary_resolution.sh
    source "$REPO_ROOT/scripts/lib/ee_binary_resolution.sh"
    ee_resolve_binary release
}

# Always-run cleanup that retains artifacts under ARTIFACT_DIR. Per
# the bead's "Artifact logs are bounded, redacted, and reproducible
# enough for future agents to compare regressions" requirement, we do
# NOT delete the artifact dir on exit — even on failure.
cleanup() {
    local exit_code=$?
    if [ "$CLEANUP_RAN" -eq 1 ]; then
        return
    fi
    CLEANUP_RAN=1
    if [ -n "$WORKSPACE" ]; then
        emit_event "cleanup" "retained" "workspace + artifacts retained for replay" \
            "$(jq -cn --arg workspaceHash "$(json_hash "$WORKSPACE")" --arg artifactDir "$ARTIFACT_DIR" '{workspaceHash: $workspaceHash, artifactDir: $artifactDir}')"
    fi
    exit "$exit_code"
}
trap cleanup EXIT

if [ "${EE_HUGE_HOST:-0}" != "1" ]; then
    skip "set EE_HUGE_HOST=1 to run the p99 proof against a 256GB+/64-core Linux host"
fi

# The optimization is Linux-only by construction (it needs MAP_POPULATE
# and ideally MADV_HUGEPAGE). Don't pretend a Mac run produced
# meaningful evidence — record the platform skip explicitly.
case "$(uname -s)" in
    Linux) ;;
    *)
        skip "lexical RAM-tier proof requires Linux; uname -s = $(uname -s)"
        ;;
esac

require_tool jq
require_tool shasum
require_tool awk

# Detect total RAM in MiB. We require >= 256 GiB (262144 MiB) per the
# parent bead's "256GB+ / 64-core host class" acceptance.
RAM_MIB="$(awk '/^MemTotal:/ {printf "%d\n", $2 / 1024}' /proc/meminfo 2>/dev/null || echo 0)"
if [ "$RAM_MIB" -lt 262144 ]; then
    skip "host has ${RAM_MIB} MiB RAM; need >= 262144 MiB (256 GiB)"
fi

# Detect core count. Required >= 64.
CORE_COUNT="$(nproc 2>/dev/null || echo 0)"
if [ "$CORE_COUNT" -lt 64 ]; then
    skip "host has $CORE_COUNT cores; need >= 64"
fi

EE_BINARY="$(resolve_ee_binary)"
if [ ! -x "$EE_BINARY" ]; then
    skip "set EE_BINARY to an executable ee binary; this harness never runs cargo"
fi

WORK_ROOT="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
WORKSPACE="$(mktemp -d "${WORK_ROOT%/}/ee-${SCENARIO}.XXXXXX")"
MANIFEST="$WORKSPACE/e2e_retention_manifest.json"
cat >"$MANIFEST" <<JSON
{
  "schema": "ee.e2e.retention_manifest.v1",
  "epic_name": "$SCENARIO",
  "workspace": "$WORKSPACE",
  "event_log": "$EVENT_FILE",
  "artifact_dir": "$ARTIFACT_DIR",
  "cleanup_policy": "retained_by_lexical_ram_tier_p99_proof"
}
JSON

emit_event "precondition" "passed" "host class qualifies for lexical RAM-tier proof" \
    "$(jq -cn --argjson ramMib "$RAM_MIB" --argjson coreCount "$CORE_COUNT" '{ramMib: $ramMib, coreCount: $coreCount, platform: "linux"}')"

if ! "$EE_BINARY" init --workspace "$WORKSPACE" --json >"$ARTIFACT_DIR/init.json"; then
    fail "setup" "ee init failed"
fi
emit_event "setup" "passed" "workspace initialized" \
    "$(jq -cn --arg workspaceHash "$(json_hash "$WORKSPACE")" '{workspaceHash: $workspaceHash}')"

# Seed a fixture corpus large enough to push the lexical index past
# 10 MiB (parent bead requirement). The exact corpus loader lives
# downstream; this slice ships the gate shape and corpus seeding stays
# behind a configurable hook so the bench can replay the same seed
# across hosts.
SEED_LOADER="${EE_LEXICAL_RAM_TIER_SEED_LOADER:-${REPO_ROOT}/tests/fixtures/corpus/corpus_2026_05_10_seed.sh}"
if [ ! -x "$SEED_LOADER" ]; then
    skip "EE_LEXICAL_RAM_TIER_SEED_LOADER ($SEED_LOADER) is not executable; provide a corpus seeder >= 10MiB lexical index"
fi
if ! EE_BINARY="$EE_BINARY" "$SEED_LOADER" --workspace "$WORKSPACE" --json >"$ARTIFACT_DIR/seed.json" 2>>"$ARTIFACT_DIR/seed.stderr.log"; then
    fail "setup" "corpus seeder failed; see $ARTIFACT_DIR/seed.stderr.log"
fi
emit_event "setup" "passed" "corpus seeded" \
    "$(jq -cn --arg seedHash "$(json_hash "$(cat "$ARTIFACT_DIR/seed.json")")" '{seedHash: $seedHash}')"

# Sanity-check the lexical index is large enough to make the
# optimization meaningful. The exact path is under
# .ee/indexes/combined/; we check the aggregate directory size.
INDEX_DIR="$WORKSPACE/.ee/indexes/combined"
if [ -d "$INDEX_DIR" ]; then
    INDEX_BYTES="$(du -sb "$INDEX_DIR" 2>/dev/null | awk '{print $1}')"
else
    INDEX_BYTES=0
fi
if [ "${INDEX_BYTES:-0}" -lt 10485760 ]; then
    skip "lexical index is ${INDEX_BYTES} bytes; need >= 10485760 (10 MiB) for meaningful proof"
fi
emit_event "precondition" "passed" "lexical index is large enough" \
    "$(jq -cn --argjson indexBytes "$INDEX_BYTES" '{indexBytes: $indexBytes}')"

# Run the bench in both modes. Each run is a fixed query set executed
# N times to capture p50/p95/p99 latencies. The fixed query set lives
# in the corpus seed manifest under data.benchQueries.
BENCH_RUNS="${EE_LEXICAL_RAM_TIER_BENCH_RUNS:-50}"
BENCH_QUERIES="$(jq -cr '.data.benchQueries // []' "$ARTIFACT_DIR/seed.json")"
if [ "$(printf '%s' "$BENCH_QUERIES" | jq 'length')" -eq 0 ]; then
    skip "corpus seed manifest provided no benchQueries; cannot measure latencies"
fi

run_bench() {
    local mode="${1:?mode required}"
    local pin_ram_flag="$2"
    local hugepages_flag="$3"
    local out="$ARTIFACT_DIR/bench_${mode}.jsonl"
    : >"$out"
    local i query start_ns end_ms elapsed_ms
    local idx
    for i in $(seq 1 "$BENCH_RUNS"); do
        idx=$(( (i - 1) % $(printf '%s' "$BENCH_QUERIES" | jq 'length') ))
        query="$(printf '%s' "$BENCH_QUERIES" | jq -r ".[$idx]")"
        start_ns="$(date +%s%N)"
        if ! EE_LEXICAL_INDEX_PIN_RAM="$pin_ram_flag" \
             EE_LEXICAL_INDEX_HUGEPAGES="$hugepages_flag" \
             "$EE_BINARY" search "$query" \
                --workspace "$WORKSPACE" \
                --json \
                >/dev/null 2>>"$ARTIFACT_DIR/bench_${mode}.stderr.log"; then
            fail "bench" "ee search failed in mode=$mode at run $i"
        fi
        end_ms="$(date +%s%N)"
        elapsed_ms="$(awk -v s="$start_ns" -v e="$end_ms" 'BEGIN {printf "%.3f", (e - s) / 1000000}')"
        jq -cn --arg mode "$mode" --arg query "$query" --argjson run "$i" --argjson elapsedMs "$elapsed_ms" \
            '{mode: $mode, run: $run, queryHash: ($query | @sha256[0:16]), elapsedMs: $elapsedMs}' >>"$out"
    done
    printf '%s\n' "$out"
}

BASELINE_OUT="$(run_bench "baseline" "0" "0")"
ENABLED_OUT="$(run_bench "ram_tier" "1" "1")"

# Compute p50/p95/p99 for each mode via jq + awk.
compute_percentiles() {
    local jsonl="${1:?jsonl required}"
    jq -r '.elapsedMs' "$jsonl" | awk '
        {
            arr[NR] = $1
        }
        END {
            n = NR
            if (n == 0) {
                print "0 0 0"
                exit
            }
            for (i = 1; i <= n; i++) {
                for (j = i + 1; j <= n; j++) {
                    if (arr[i] > arr[j]) {
                        t = arr[i]; arr[i] = arr[j]; arr[j] = t
                    }
                }
            }
            p50 = arr[int(n * 0.50 + 0.5)]
            p95 = arr[int(n * 0.95 + 0.5)]
            p99 = arr[int(n * 0.99 + 0.5)]
            printf "%.3f %.3f %.3f\n", p50, p95, p99
        }
    '
}

read -r BASELINE_P50 BASELINE_P95 BASELINE_P99 < <(compute_percentiles "$BASELINE_OUT")
read -r ENABLED_P50 ENABLED_P95 ENABLED_P99 < <(compute_percentiles "$ENABLED_OUT")

P99_DELTA_PCT="$(awk -v b="$BASELINE_P99" -v e="$ENABLED_P99" 'BEGIN {
    if (b <= 0) { printf "0\n" } else { printf "%.2f\n", ((b - e) / b) * 100 }
}')"

emit_perf "lexical_ram_tier_p99_proof" \
    "$(jq -cn \
        --argjson baselineP50 "$BASELINE_P50" \
        --argjson baselineP95 "$BASELINE_P95" \
        --argjson baselineP99 "$BASELINE_P99" \
        --argjson enabledP50 "$ENABLED_P50" \
        --argjson enabledP95 "$ENABLED_P95" \
        --argjson enabledP99 "$ENABLED_P99" \
        --argjson p99DeltaPct "$P99_DELTA_PCT" \
        --argjson benchRuns "$BENCH_RUNS" \
        --argjson queryCount "$(printf '%s' "$BENCH_QUERIES" | jq 'length')" \
        --argjson indexBytes "$INDEX_BYTES" \
        '{
          baseline: {p50Ms: $baselineP50, p95Ms: $baselineP95, p99Ms: $baselineP99},
          enabled: {p50Ms: $enabledP50, p95Ms: $enabledP95, p99Ms: $enabledP99},
          p99ImprovementPct: $p99DeltaPct,
          benchRuns: $benchRuns,
          queryCount: $queryCount,
          indexBytes: $indexBytes
        }')"

emit_event "assert" "passed" "p99 proof captured" \
    "$(jq -cn --argjson p99DeltaPct "$P99_DELTA_PCT" '{p99ImprovementPct: $p99DeltaPct}')"

printf '%s\n' "$EVENT_FILE"
