#!/usr/bin/env bash
# bd-1zb7k.12.2 — deterministic host calibration harness.
#
# This is invoked-only. It never runs from normal ee commands and never builds
# Rust locally. Provide EE_BINARY or prebuild the release binary through RCH.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

PROFILE="smoke"
JSON_OUTPUT=0
OUTPUT_FILE=""
FAILED=0

usage() {
    cat <<'EOF'
Usage: scripts/e2e_overhaul/host_calibration.sh [--profile smoke|stress] [--json] [--output PATH]

Runs a deterministic synthetic calibration workspace and emits an ee.perf.v1
artifact covering host-profile, index, search, context, pack, graph, and
renderer timing paths. The default smoke profile is intentionally cheap.

Stress profile is opt-in only:
  EE_HOST_CALIBRATION_ALLOW_STRESS=1 scripts/e2e_overhaul/host_calibration.sh --profile stress

When EE_HOST_CALIBRATION_REQUIRE_RCH=1 is set, stress mode also requires
RCH_REQUIRE_REMOTE=1 so agents do not accidentally run heavyweight local work.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --profile)
            PROFILE="${2:?--profile requires a value}"
            shift 2
            ;;
        --profile=*)
            PROFILE="${1#--profile=}"
            shift
            ;;
        --json)
            JSON_OUTPUT=1
            shift
            ;;
        --output)
            OUTPUT_FILE="${2:?--output requires a value}"
            shift 2
            ;;
        --output=*)
            OUTPUT_FILE="${1#--output=}"
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "host-calibration: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$PROFILE" in
    smoke)
        PROFILE_CLASS="portable_smoke"
        WORKLOAD_TIER="small"
        MEMORY_COUNT="${EE_HOST_CALIBRATION_MEMORY_COUNT:-12}"
        ;;
    stress)
        if [ "${EE_HOST_CALIBRATION_ALLOW_STRESS:-0}" != "1" ]; then
            echo "host-calibration: stress profile requires EE_HOST_CALIBRATION_ALLOW_STRESS=1" >&2
            exit 2
        fi
        if [ "${EE_HOST_CALIBRATION_REQUIRE_RCH:-0}" = "1" ] && [ "${RCH_REQUIRE_REMOTE:-0}" != "1" ]; then
            echo "host-calibration: stress profile requires RCH_REQUIRE_REMOTE=1 when remote execution is required" >&2
            exit 2
        fi
        PROFILE_CLASS="local_256gb_opt_in"
        WORKLOAD_TIER="stress"
        MEMORY_COUNT="${EE_HOST_CALIBRATION_MEMORY_COUNT:-500}"
        ;;
    *)
        echo "host-calibration: unknown profile: $PROFILE" >&2
        exit 2
        ;;
esac

require_jq
epic_setup "host_calibration"

ARTIFACT_DIR="$EPIC_WORKSPACE/host-calibration-artifacts"
mkdir -p "$ARTIFACT_DIR"
OPERATIONS_JSONL="$ARTIFACT_DIR/operations.jsonl"
QUERY_FILE="$ARTIFACT_DIR/calibration-query.eeq.json"
TIMESTAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
GIT_SHA="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || printf 'unknown')"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | sed -n '1p')"

now_ns() {
    python3 -c 'import time; print(time.monotonic_ns())'
}

elapsed_ms() {
    python3 - "$1" "$2" <<'PY'
import sys
start = int(sys.argv[1])
end = int(sys.argv[2])
print(f"{(end - start) / 1_000_000:.6f}")
PY
}

emit_calibration_event() {
    local operation="$1"
    local status="$2"
    local elapsed="$3"
    local rc="$4"
    if [ -z "${EE_TEST_LOG_PATH:-}" ]; then
        return 0
    fi
    mkdir -p "$(dirname "$EE_TEST_LOG_PATH")"
    jq -cn \
        --arg ts "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
        --arg operation "$operation" \
        --arg status "$status" \
        --arg elapsed "$elapsed" \
        --arg rc "$rc" \
        --arg profile "$PROFILE" \
        '{
          schema: "ee.test_event.v1",
          ts: $ts,
          test_id: "bd-1zb7k.12.2",
          kind: "host_calibration",
          fields: {
            bead_id: "bd-1zb7k.12.2",
            surface: "host_calibration",
            phase: $operation,
            status: $status,
            elapsed_ms: ($elapsed | tonumber),
            exit_code: ($rc | tonumber),
            profile: $profile
          }
        }' >>"$EE_TEST_LOG_PATH"
}

append_operation() {
    local operation="$1"
    local status="$2"
    local elapsed="$3"
    local rc="$4"
    jq -cn \
        --arg operation "$operation" \
        --arg status "$status" \
        --arg profile "$PROFILE" \
        --arg tier "$WORKLOAD_TIER" \
        --arg elapsed "$elapsed" \
        --arg rc "$rc" \
        '{
          key: $operation,
          value: {
            status: $status,
            profile: $profile,
            workload_tier: $tier,
            p50_ms: ($elapsed | tonumber),
            p95_ms: ($elapsed | tonumber),
            p99_ms: ($elapsed | tonumber),
            samples_count: 1,
            max_ms: ($elapsed | tonumber),
            max_rss_kb: null,
            allocation_count: null,
            db_size_bytes: null,
            index_size_bytes: null,
            rows_per_sec: null,
            regression_status: "not_checked",
            baseline_ref: {
              file: "benches/baselines/v0.1.json",
              operation: $operation
            },
            budget_mode: "advisory",
            exit_code: ($rc | tonumber)
          }
        }' >>"$OPERATIONS_JSONL"
    emit_calibration_event "$operation" "$status" "$elapsed" "$rc"
}

run_stage() {
    local operation="$1"
    shift
    local stdout_file="$ARTIFACT_DIR/$operation.stdout"
    local stderr_file="$ARTIFACT_DIR/$operation.stderr"
    local start end elapsed rc status
    start="$(now_ns)"
    "$@" >"$stdout_file" 2>"$stderr_file"
    rc=$?
    end="$(now_ns)"
    elapsed="$(elapsed_ms "$start" "$end")"
    if [ "$rc" -eq 0 ]; then
        status="measured"
    else
        status="failed"
        FAILED=1
    fi
    append_operation "$operation" "$status" "$elapsed" "$rc"
    return "$rc"
}

seed_fixture() {
    local i
    for i in $(seq 1 "$MEMORY_COUNT"); do
        run_stage "seed_memory_$i" "$EE_BINARY" remember \
            --workspace "$EPIC_WORKSPACE" --json \
            --level procedural --kind rule --tags "host-calibration,bd-1zb7k.12.2" \
            "Host calibration deterministic memory $i: search context pack index graph renderer stage timing." \
            >/dev/null || return 1
    done
}

cat >"$QUERY_FILE" <<'EOF'
{
  "version": "ee.query.v1",
  "query": { "text": "host calibration deterministic stage timing" },
  "budget": { "maxTokens": 1200, "candidatePool": 20 },
  "output": { "profile": "compact" }
}
EOF

run_stage "host_profile" "$EE_BINARY" diag host-profile --workspace "$EPIC_WORKSPACE" --json
seed_fixture
run_stage "index_rebuild" "$EE_BINARY" index rebuild --workspace "$EPIC_WORKSPACE" --json
run_stage "search_json" "$EE_BINARY" search "host calibration deterministic stage timing" --workspace "$EPIC_WORKSPACE" --json
run_stage "context_json" "$EE_BINARY" context "host calibration deterministic stage timing" --workspace "$EPIC_WORKSPACE" --max-tokens 1200 --json
run_stage "pack_query_file" "$EE_BINARY" pack --workspace "$EPIC_WORKSPACE" --query-file "$QUERY_FILE" --json
run_stage "graph_snapshot_dry_run" "$EE_BINARY" graph snapshot refresh --workspace "$EPIC_WORKSPACE" --dry-run --json
run_stage "renderer_markdown" "$EE_BINARY" context "host calibration deterministic stage timing" --workspace "$EPIC_WORKSPACE" --max-tokens 1200 --format markdown

OPERATIONS_JSON="$(jq -s 'map({(.key): .value}) | add // {}' "$OPERATIONS_JSONL")"
PERF_JSON="$(jq -cn \
    --arg profile "$PROFILE" \
    --arg profile_class "$PROFILE_CLASS" \
    --arg timestamp "$TIMESTAMP" \
    --arg version "${VERSION:-0.1.0}" \
    --arg git_sha "$GIT_SHA" \
    --arg target_dir "${CARGO_TARGET_DIR:-}" \
    --arg artifact_dir "$ARTIFACT_DIR" \
    --arg tier "$WORKLOAD_TIER" \
    --arg memory_count "$MEMORY_COUNT" \
    --argjson operations "$OPERATIONS_JSON" \
    '{
      schema: "ee.perf.v1",
      profile: $profile,
      profile_class: $profile_class,
      timestamp: $timestamp,
      version: $version,
      git_sha: $git_sha,
      target_dir: $target_dir,
      criterion_dir: null,
      artifact_dir: $artifact_dir,
      budget_mode: "advisory",
      release_blocking: false,
      artifact_redaction: {
        status: "redaction_safe",
        raw_secret_material: "not_used",
        policy: "deterministic synthetic fixture only"
      },
      workload: {
        schema: "ee.perf.workload_ref.v1",
        manifest: "scripts/e2e_overhaul/host_calibration.sh",
        tier: $tier,
        memory_count: ($memory_count | tonumber)
      },
      operations: $operations,
      budgets_file: "benches/budgets.toml",
      baseline_file: "benches/baselines/v0.1.json"
    }')"

if [ "$JSON_OUTPUT" -eq 1 ]; then
    printf '%s\n' "$PERF_JSON"
else
    if [ -z "$OUTPUT_FILE" ]; then
        OUTPUT_FILE="$ARTIFACT_DIR/host-calibration-ee-perf.v1.json"
    fi
    mkdir -p "$(dirname "$OUTPUT_FILE")"
    printf '%s\n' "$PERF_JSON" >"$OUTPUT_FILE"
    echo "host-calibration: wrote $OUTPUT_FILE" >&2
fi

exit "$FAILED"
