#!/usr/bin/env bash
# I4 - No-live-service swarm incident drill harness.
#
# Replays committed synthetic incident fixtures through `ee diag incident`.
# The drill is intentionally read-only: no live Agent Mail, no Beads mutation,
# no RCH build, no local Cargo fallback, and no deletion.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
require_ee_binary

FIXTURE_DIR="$REPO_ROOT/tests/fixtures/swarm_incidents"
RUNS_PER_FIXTURE="${SWARM_INCIDENT_DRILL_RUNS:-3}"
EVENT_DIR="${EE_TEST_EVENT_DIR:-${TMPDIR:-/tmp}/ee-swarm-incident-drill-events}"
ARTIFACT_DIR="$EVENT_DIR/artifacts"
DRILL_LOG="$EVENT_DIR/swarm_incident_drill.jsonl"
SUMMARY_JSON="$EVENT_DIR/swarm_incident_drill_summary.json"
LATENCIES_TSV="$EVENT_DIR/swarm_incident_drill_latencies.tsv"

mkdir -p "$ARTIFACT_DIR"
: > "$DRILL_LOG"
: > "$LATENCIES_TSV"

e2e_log_start "swarm_incident_drill" "$DRILL_LOG"

now_ns() {
    python3 -c 'import time; print(time.time_ns())'
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print $1}'
}

normalize_replay() {
    jq -S '
      del(.data.fixture.path)
      | del(.meta)
      | del(.diagnostics)
    ' "$1"
}

emit_drill_event() {
    local scenario_id="$1"
    local run_index="$2"
    local exit_code="$3"
    local elapsed_ms="$4"
    local output_hash="$5"
    local recovery_action_count="$6"
    local stdout_path="$7"
    local stderr_path="$8"
    _e2e_emit_event "incident_drill_replay" \
        "scenario_id" "$scenario_id" \
        "run_index" "$run_index" \
        "command" "$EE_BINARY diag incident --fixture <fixture> --json" \
        "exit_code" "$exit_code" \
        "elapsed_ms" "$elapsed_ms" \
        "output_hash" "$output_hash" \
        "recovery_action_count" "$recovery_action_count" \
        "stdout_path" "$stdout_path" \
        "stderr_path" "$stderr_path"
}

if [ "$RUNS_PER_FIXTURE" -lt 2 ]; then
    echo "swarm_incident_drill: SWARM_INCIDENT_DRILL_RUNS must be >= 2" >&2
    exit 2
fi

if [ ! -d "$FIXTURE_DIR" ]; then
    echo "swarm_incident_drill: missing fixture directory: $FIXTURE_DIR" >&2
    exit 2
fi

fixture_count=0
run_count=0
failed=0

while IFS= read -r fixture; do
    fixture_count=$((fixture_count + 1))
    fixture_rel="${fixture#"$REPO_ROOT"/}"
    scenario_id="$(jq -r '.scenarioId // empty' "$fixture")"
    if [ -z "$scenario_id" ]; then
        echo "swarm_incident_drill: fixture missing scenarioId: $fixture_rel" >&2
        exit 1
    fi

    baseline_hash=""
    run_index=1
    while [ "$run_index" -le "$RUNS_PER_FIXTURE" ]; do
        stdout_path="$ARTIFACT_DIR/${scenario_id}.run${run_index}.stdout.json"
        stderr_path="$ARTIFACT_DIR/${scenario_id}.run${run_index}.stderr.txt"
        normalized_path="$ARTIFACT_DIR/${scenario_id}.run${run_index}.normalized.json"

        start_ns="$(now_ns)"
        set +e
        (
            cd "$REPO_ROOT"
            "$EE_BINARY" diag incident --fixture "$fixture_rel" --json
        ) >"$stdout_path" 2>"$stderr_path"
        exit_code=$?
        set -e
        end_ns="$(now_ns)"
        elapsed_ms="$(python3 - "$start_ns" "$end_ns" <<'PY'
import sys
start = int(sys.argv[1])
end = int(sys.argv[2])
print(f"{(end - start) / 1_000_000:.3f}")
PY
)"

        if [ "$exit_code" -ne 0 ]; then
            emit_drill_event "$scenario_id" "$run_index" "$exit_code" "$elapsed_ms" \
                "unavailable" "0" "$stdout_path" "$stderr_path"
            echo "swarm_incident_drill: replay failed for $scenario_id run $run_index" >&2
            failed=1
            break
        fi

        normalize_replay "$stdout_path" > "$normalized_path"
        output_hash="$(sha256_file "$normalized_path")"
        recovery_action_count="$(jq -r '.data.recoveryActions | length' "$stdout_path")"
        printf '%s\t%s\t%s\n' "$scenario_id" "$run_index" "$elapsed_ms" >> "$LATENCIES_TSV"
        emit_drill_event "$scenario_id" "$run_index" "$exit_code" "$elapsed_ms" \
            "$output_hash" "$recovery_action_count" "$stdout_path" "$stderr_path"

        if [ -z "$baseline_hash" ]; then
            baseline_hash="$output_hash"
        elif [ "$output_hash" != "$baseline_hash" ]; then
            echo "swarm_incident_drill: nondeterministic replay for $scenario_id: $baseline_hash != $output_hash" >&2
            failed=1
            break
        fi

        run_count=$((run_count + 1))
        run_index=$((run_index + 1))
    done

    if [ "$failed" -ne 0 ]; then
        break
    fi
done < <(find "$FIXTURE_DIR" -maxdepth 1 -type f -name '*.json' ! -name 'unsafe_recovery_actions.json' | sort)

if [ "$fixture_count" -eq 0 ]; then
    echo "swarm_incident_drill: no incident fixtures found under $FIXTURE_DIR" >&2
    exit 1
fi

python3 - "$LATENCIES_TSV" "$SUMMARY_JSON" "$fixture_count" "$run_count" "$RUNS_PER_FIXTURE" "$failed" <<'PY'
import json
import math
import sys
from pathlib import Path

latency_path, summary_path, fixture_count, run_count, runs_per_fixture, failed = sys.argv[1:]
fixture_count = int(fixture_count)
run_count = int(run_count)
runs_per_fixture = int(runs_per_fixture)
failed = int(failed)

latencies = []
scenario_counts = {}
for line in Path(latency_path).read_text(encoding="utf-8").splitlines():
    if not line.strip():
        continue
    scenario, _run, elapsed = line.split("\t")
    latencies.append(float(elapsed))
    scenario_counts[scenario] = scenario_counts.get(scenario, 0) + 1

def percentile(values, pct):
    if not values:
        return 0.0
    ordered = sorted(values)
    index = math.ceil((pct / 100.0) * len(ordered)) - 1
    index = max(0, min(index, len(ordered) - 1))
    return ordered[index]

summary = {
    "schema": "ee.swarm_incident_drill.v1",
    "success": failed == 0,
    "fixtureCount": fixture_count,
    "runsPerFixture": runs_per_fixture,
    "runCount": run_count,
    "scenarioCounts": dict(sorted(scenario_counts.items())),
    "latency": {
        "p95Ms": percentile(latencies, 95),
        "p99Ms": percentile(latencies, 99),
        "maxMs": max(latencies) if latencies else 0.0,
    },
    "safety": {
        "noLiveServices": True,
        "noBeadsMutation": True,
        "noRchBuild": True,
        "noLocalCargo": True,
        "noDeletion": True,
    },
}
Path(summary_path).write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY

summary_success="$(jq -r '.success' "$SUMMARY_JSON")"
_e2e_emit_event "incident_drill_summary" \
    "summary_path" "$SUMMARY_JSON" \
    "fixture_count" "$(jq -r '.fixtureCount' "$SUMMARY_JSON")" \
    "run_count" "$(jq -r '.runCount' "$SUMMARY_JSON")" \
    "p95_ms" "$(jq -r '.latency.p95Ms' "$SUMMARY_JSON")" \
    "p99_ms" "$(jq -r '.latency.p99Ms' "$SUMMARY_JSON")" \
    "success" "$summary_success"
e2e_log_end

cat "$SUMMARY_JSON"

if [ "$summary_success" != "true" ]; then
    exit 1
fi
