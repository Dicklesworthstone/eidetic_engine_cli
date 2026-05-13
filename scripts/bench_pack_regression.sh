#!/bin/sh
set -eu

# Pack-size regression gate for J5.
#
# Usage:
#   ./scripts/bench_pack_regression.sh
#   ./scripts/bench_pack_regression.sh --json
#   ./scripts/bench_pack_regression.sh --skip-run --summary target/criterion/pack_size/summary.json

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"

BASELINE_FILE="$PROJECT_ROOT/benches/baselines/pack_size_v0_1.json"
SKIP_RUN=false
JSON_OUTPUT=false

if [ -d "$DEFAULT_AGENT_BUILD_ROOT" ]; then
    mkdir -p "$DEFAULT_AGENT_BUILD_ROOT/cargo-target" "$DEFAULT_AGENT_BUILD_ROOT/tmp" 2>/dev/null || true
    export TMPDIR="${EE_AGENT_TMPDIR:-$DEFAULT_AGENT_BUILD_ROOT/tmp}"
fi
if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    TARGET_ROOT="$CARGO_TARGET_DIR"
elif [ -d "$DEFAULT_AGENT_BUILD_ROOT" ]; then
    TARGET_ROOT="$DEFAULT_AGENT_BUILD_ROOT/cargo-target"
else
    TARGET_ROOT="${TMPDIR:-/tmp}/rch_target_ee_pack_size"
fi
export CARGO_TARGET_DIR="$TARGET_ROOT"
SUMMARY_FILE="$TARGET_ROOT/criterion/pack_size/summary.json"

usage() {
    sed -n '3,11p' "$0" | sed 's/^# //' | sed 's/^#//'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --baseline)
            shift
            if [ "$#" -eq 0 ]; then
                echo "Missing value for --baseline" >&2
                exit 1
            fi
            BASELINE_FILE="$1"
            ;;
        --summary)
            shift
            if [ "$#" -eq 0 ]; then
                echo "Missing value for --summary" >&2
                exit 1
            fi
            SUMMARY_FILE="$1"
            ;;
        --skip-run)
            SKIP_RUN=true
            ;;
        --json)
            JSON_OUTPUT=true
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 1
            ;;
    esac
    shift
done

if ! command -v jq >/dev/null 2>&1; then
    echo "error: jq is required for pack-size regression checks" >&2
    exit 1
fi

if [ ! -f "$BASELINE_FILE" ]; then
    echo "error: baseline not found: $BASELINE_FILE" >&2
    exit 1
fi

if [ "$SKIP_RUN" != "true" ]; then
    echo "[*] Running pack-size measurement benchmark..." >&2
    cargo bench --bench pack_size -- --measure-only >&2
fi

if [ ! -f "$SUMMARY_FILE" ]; then
    echo "error: pack-size summary not found: $SUMMARY_FILE" >&2
    echo "       run: cargo bench --bench pack_size -- --measure-only" >&2
    exit 1
fi

json_value() {
    file="$1"
    filter="$2"
    value=$(jq -r "$filter // empty" "$file")
    if [ -z "$value" ] || [ "$value" = "null" ]; then
        echo "error: missing JSON field '$filter' in $file" >&2
        exit 1
    fi
    printf '%s\n' "$value"
}

current_json=$(json_value "$SUMMARY_FILE" '.measurements.pack_json_bytes_for_1000_tokens.value')
baseline_json=$(json_value "$BASELINE_FILE" '.measurements.pack_json_bytes_for_1000_tokens.value')
current_bpt=$(json_value "$SUMMARY_FILE" '.measurements.bytes_per_token.value')
baseline_bpt=$(json_value "$BASELINE_FILE" '.measurements.bytes_per_token.value')

json_growth_max=$(json_value "$BASELINE_FILE" '.thresholds.pack_json_bytes_for_1000_tokens_growth_max')
bpt_growth_max=$(json_value "$BASELINE_FILE" '.thresholds.bytes_per_token_growth_max')

allowed_json=$(awk -v baseline="$baseline_json" -v growth="$json_growth_max" 'BEGIN { printf "%.6f", baseline * growth }')
allowed_bpt=$(awk -v baseline="$baseline_bpt" -v growth="$bpt_growth_max" 'BEGIN { printf "%.6f", baseline * growth }')

json_failed=$(awk -v current="$current_json" -v allowed="$allowed_json" 'BEGIN { print (current > allowed) ? "true" : "false" }')
bpt_failed=$(awk -v current="$current_bpt" -v allowed="$allowed_bpt" 'BEGIN { print (current > allowed) ? "true" : "false" }')

status="pass"
if [ "$json_failed" = "true" ] || [ "$bpt_failed" = "true" ]; then
    status="fail"
fi

report=$(jq -n \
    --arg schema "ee.bench.pack_size.regression.v1" \
    --arg status "$status" \
    --arg baseline "$BASELINE_FILE" \
    --arg summary "$SUMMARY_FILE" \
    --argjson current_json "$current_json" \
    --argjson baseline_json "$baseline_json" \
    --argjson allowed_json "$allowed_json" \
    --argjson current_bpt "$current_bpt" \
    --argjson baseline_bpt "$baseline_bpt" \
    --argjson allowed_bpt "$allowed_bpt" \
    --argjson json_failed "$json_failed" \
    --argjson bpt_failed "$bpt_failed" \
    '{
      schema: $schema,
      status: $status,
      baseline: $baseline,
      summary: $summary,
      checks: {
        pack_json_bytes_for_1000_tokens: {
          current: $current_json,
          baseline: $baseline_json,
          allowed: $allowed_json,
          failed: $json_failed
        },
        bytes_per_token: {
          current: $current_bpt,
          baseline: $baseline_bpt,
          allowed: $allowed_bpt,
          failed: $bpt_failed
        }
      }
    }')

if [ "$JSON_OUTPUT" = "true" ]; then
    printf '%s\n' "$report"
else
    echo "[+] pack_json_bytes_for_1000_tokens: current=$current_json baseline=$baseline_json allowed=$allowed_json" >&2
    echo "[+] bytes_per_token: current=$current_bpt baseline=$baseline_bpt allowed=$allowed_bpt" >&2
fi

if [ "$status" != "pass" ]; then
    echo "[-] Pack-size regression detected" >&2
    exit 1
fi

echo "[+] Pack-size regression gate passed" >&2
exit 0
