#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="$REPO_ROOT/tests/fixtures/golden/swarm_next_action"
EVENT_DIR="${TMPDIR:-/tmp}/ee-swarm-next-action-profile"
EVENT_LOG="$EVENT_DIR/events.jsonl"
SUMMARY_JSON="$EVENT_DIR/summary.json"
ITERATIONS="${EE_SWA7_ITERATIONS:-5}"
PROFILE_MODE="${EE_SWA7_PROFILE_MODE:-0}"

mkdir -p "$EVENT_DIR"
: > "$EVENT_LOG"

if ! command -v python3 >/dev/null 2>&1; then
  printf 'error: python3 is required for swarm next-action profile harness\n' >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  printf 'error: jq is required for swarm next-action profile harness\n' >&2
  exit 1
fi

FIXTURES=()
while IFS= read -r fixture; do
  FIXTURES+=("$fixture")
done < <(find "$FIXTURE_DIR" -maxdepth 1 -type f -name '*.json.golden' -print | sort)

if [ "${#FIXTURES[@]}" -eq 0 ]; then
  printf 'error: no swarm next-action golden fixtures found in %s\n' "$FIXTURE_DIR" >&2
  exit 1
fi

python3 - "$REPO_ROOT" "$EVENT_DIR" "$EVENT_LOG" "$SUMMARY_JSON" "$ITERATIONS" "$PROFILE_MODE" "${FIXTURES[@]}" <<'PY'
import hashlib
import json
import os
import statistics
import sys
import time
from pathlib import Path

(
    repo_root,
    event_dir,
    event_log,
    summary_json,
    iterations_raw,
    profile_mode_raw,
    *fixture_paths,
) = sys.argv[1:]

repo_root = Path(repo_root)
event_dir = Path(event_dir)
iterations = int(iterations_raw)
profile_mode = profile_mode_raw == "1"

if iterations < 2:
    raise SystemExit("EE_SWA7_ITERATIONS must be >= 2 for p50/p95/p99 evidence")


def sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def percentile(values, pct):
    ordered = sorted(values)
    if not ordered:
        return 0.0
    index = int(round((len(ordered) - 1) * (pct / 100.0)))
    return ordered[index]


def rel(path: Path) -> str:
    try:
        return str(path.relative_to(repo_root))
    except ValueError:
        return str(path)


def canonicalize(value) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def validate_response(value):
    schema = value.get("schema")
    if schema in {"ee.response.v1", "ee.response.v2"}:
        if value.get("success") is not True:
            return False, "response is not successful"
        data = value.get("data")
    elif schema == "ee.swarm_next_action.v1":
        data = value
    else:
        return False, "top-level schema is not ee.response.v1, ee.response.v2, or ee.swarm_next_action.v1"
    if not isinstance(data, dict):
        return False, "data is not an object"
    if data.get("schema") != "ee.swarm_next_action.v1":
        return False, "data.schema is not ee.swarm_next_action.v1"
    cards = data.get("recommendationCards")
    if not isinstance(cards, list):
        return False, "recommendationCards is not an array"
    return True, ""


def timed_load_validate_canonicalize(path: Path):
    phases = {}

    start = time.perf_counter_ns()
    raw = path.read_bytes()
    phases["readFixtureMs"] = (time.perf_counter_ns() - start) / 1_000_000.0

    start = time.perf_counter_ns()
    value = json.loads(raw)
    phases["parseJsonMs"] = (time.perf_counter_ns() - start) / 1_000_000.0

    start = time.perf_counter_ns()
    schema_ok, diagnosis = validate_response(value)
    phases["schemaValidateMs"] = (time.perf_counter_ns() - start) / 1_000_000.0

    start = time.perf_counter_ns()
    canonical = canonicalize(value)
    phases["canonicalizeJsonMs"] = (time.perf_counter_ns() - start) / 1_000_000.0

    start = time.perf_counter_ns()
    output_hash = sha256_bytes(canonical)
    phases["hashOutputMs"] = (time.perf_counter_ns() - start) / 1_000_000.0

    elapsed_ms = sum(phases.values())
    return raw, value, canonical, output_hash, schema_ok, diagnosis, elapsed_ms, phases


events = []
summary = {
    "schema": "ee.swarm_next_action.profile_harness.v1",
    "beadId": "bd-3vwx0.7",
    "profileMode": profile_mode,
    "iterations": iterations,
    "cwd": str(repo_root),
    "sanitizedEnv": {
        "EE_SWA7_ITERATIONS": str(iterations),
        "EE_SWA7_PROFILE_MODE": "1" if profile_mode else "0",
        "TMPDIR": os.environ.get("TMPDIR", "/tmp"),
    },
    "fixtures": [],
    "opportunityMatrix": [],
}

phase_totals = {
    "readFixtureMs": [],
    "parseJsonMs": [],
    "schemaValidateMs": [],
    "canonicalizeJsonMs": [],
    "hashOutputMs": [],
}

for fixture_raw in fixture_paths:
    fixture = Path(fixture_raw)
    fixture_name = fixture.name.removesuffix(".json.golden")
    stdout_before = event_dir / f"{fixture_name}.before.canonical.json"
    stdout_after = event_dir / f"{fixture_name}.after.canonical.json"
    stderr_artifact = event_dir / f"{fixture_name}.stderr.txt"
    stderr_artifact.write_text("", encoding="utf-8")

    elapsed_samples = []
    first_failure = ""
    schema_validated = False
    output_hashes = []
    fixture_hash = ""

    for iteration in range(iterations):
        raw, value, canonical, output_hash, schema_ok, diagnosis, elapsed_ms, phases = (
            timed_load_validate_canonicalize(fixture)
        )
        fixture_hash = sha256_bytes(raw)
        elapsed_samples.append(elapsed_ms)
        output_hashes.append(output_hash)
        schema_validated = schema_validated or schema_ok
        if not schema_ok and not first_failure:
            first_failure = diagnosis
        for key, value_ms in phases.items():
            phase_totals[key].append(value_ms)
        if iteration == 0:
            stdout_before.write_bytes(canonical)
        if iteration == iterations - 1:
            stdout_after.write_bytes(canonical)

    before_hash = sha256_bytes(stdout_before.read_bytes())
    after_hash = sha256_bytes(stdout_after.read_bytes())
    output_hash = output_hashes[0]
    stable_output = all(candidate == output_hash for candidate in output_hashes)
    isomorphism_stable = before_hash == after_hash == output_hash and stable_output

    if not isomorphism_stable and not first_failure:
        first_failure = "canonical output hash changed across no-op iterations"

    p50 = percentile(elapsed_samples, 50)
    p95 = percentile(elapsed_samples, 95)
    p99 = percentile(elapsed_samples, 99)
    mean = statistics.fmean(elapsed_samples)

    event = {
        "schema": "ee.test_event.v1",
        "kind": "swarm_next_action_profile",
        "beadId": "bd-3vwx0.7",
        "fixture": rel(fixture),
        "command": "canonicalize swarm next-action golden fixture",
        "cwd": str(repo_root),
        "sanitizedEnv": summary["sanitizedEnv"],
        "fixtureHash": fixture_hash,
        "outputHash": output_hash,
        "elapsedMs": {
            "p50": round(p50, 3),
            "p95": round(p95, 3),
            "p99": round(p99, 3),
            "mean": round(mean, 3),
        },
        "stdoutArtifact": str(stdout_after),
        "stderrArtifact": str(stderr_artifact),
        "schemaValidated": schema_validated,
        "isomorphismStable": isomorphism_stable,
        "firstFailureDiagnosis": first_failure,
    }
    events.append(event)
    summary["fixtures"].append(event)

if profile_mode:
    ranked_phases = sorted(
        (
            {
                "hotspot": phase,
                "impact": round(percentile(samples, 95), 3),
                "confidence": "fixture_timing",
                "effort": "measurement_only",
                "score": round(percentile(samples, 95) / 10.0, 3),
                "proofPlan": "rerun this harness and compare isomorphismStable plus outputHash",
            }
            for phase, samples in phase_totals.items()
        ),
        key=lambda item: item["impact"],
        reverse=True,
    )
    summary["opportunityMatrix"] = ranked_phases[:5]

with open(event_log, "a", encoding="utf-8") as handle:
    for event in events:
        handle.write(json.dumps(event, sort_keys=True, separators=(",", ":")) + "\n")

Path(summary_json).write_text(
    json.dumps(summary, sort_keys=True, indent=2) + "\n",
    encoding="utf-8",
)

failures = [event for event in events if not event["schemaValidated"] or not event["isomorphismStable"]]
if failures:
    print(json.dumps({"failures": failures}, sort_keys=True, indent=2), file=sys.stderr)
    raise SystemExit(1)
PY

jq -e '
  .schema == "ee.swarm_next_action.profile_harness.v1"
  and (.fixtures | length) > 0
  and all(.fixtures[]; .schemaValidated and .isomorphismStable)
' "$SUMMARY_JSON" >/dev/null

if [ "$PROFILE_MODE" = "1" ]; then
  jq -e '(.opportunityMatrix | length) > 0 and (.opportunityMatrix | length) <= 5' \
    "$SUMMARY_JSON" >/dev/null
fi

printf 'swarm next-action profile harness passed; events=%s summary=%s\n' \
  "$EVENT_LOG" "$SUMMARY_JSON" >&2
