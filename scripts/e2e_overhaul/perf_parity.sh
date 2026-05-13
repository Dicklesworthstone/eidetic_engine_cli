#!/usr/bin/env bash
# J10 — README performance claims parity smoke.
#
# Runs the light benchmark profile, validates the ee.perf.v1 envelope fields
# that agents consume, and prints a compact operation summary to stderr.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"

if [ -d "$DEFAULT_AGENT_BUILD_ROOT" ]; then
    mkdir -p "$DEFAULT_AGENT_BUILD_ROOT/cargo-target" "$DEFAULT_AGENT_BUILD_ROOT/tmp" 2>/dev/null || true
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$DEFAULT_AGENT_BUILD_ROOT/cargo-target}"
    export TMPDIR="${EE_AGENT_TMPDIR:-$DEFAULT_AGENT_BUILD_ROOT/tmp}"
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "perf-parity: jq is required" >&2
    exit 2
fi

if [ "$#" -eq 0 ]; then
    set -- --profile ci-smoke --json
fi

perf_json="$("$REPO_ROOT/scripts/bench_perf_regression.sh" "$@")"

printf '%s\n' "$perf_json" | jq -e '
  .schema == "ee.perf.v1"
  and (.profile | type == "string")
  and (.workload | type == "object")
  and (.operations | type == "object")
  and (.baseline_file | type == "string")
' >/dev/null

printf '%s\n' "$perf_json" | jq -e '
  [
    .operations
    | to_entries[]
    | .value as $operation
    | select(
        (
          ($operation | has("p50_ms")) and
          ($operation | has("p99_ms")) and
          ($operation | has("samples_count")) and
          ($operation.regression_status | type == "string") and
          ($operation.baseline_ref.file | type == "string") and
          ($operation.baseline_ref.operation | type == "string")
        )
        | not
      )
  ]
  | length == 0
' >/dev/null

echo "operation                           p50       p99       regression" >&2
printf '%s\n' "$perf_json" | jq -r '
  .operations
  | to_entries[]
  | [
      .key,
      (.value.p50_ms | tostring),
      (.value.p99_ms | tostring),
      .value.regression_status
    ]
  | @tsv
' | while IFS="$(printf '\t')" read -r operation p50 p99 regression; do
    printf '%-35s %-9s %-9s %s\n' "$operation" "$p50" "$p99" "$regression" >&2
done
