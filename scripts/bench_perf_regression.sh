#!/bin/sh
set -eu

# J9 compatibility wrapper for the broader performance regression suite.
# The canonical runner is scripts/bench.sh; this script pins the J9 baseline
# file named in the bead and forwards all profile/check options to bench.sh.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
BASELINE_FILE="$PROJECT_ROOT/benches/baselines/perf_v0_2.json"

if [ ! -f "$BASELINE_FILE" ]; then
    echo "Error: J9 performance baseline missing at $BASELINE_FILE" >&2
    exit 1
fi

if [ "$#" -eq 0 ]; then
    set -- --profile nightly --check-regression
fi

EE_BENCH_BASELINE_FILE="$BASELINE_FILE" \
    "$SCRIPT_DIR/bench.sh" "$@"
