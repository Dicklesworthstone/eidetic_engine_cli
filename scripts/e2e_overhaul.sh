#!/usr/bin/env bash
# J4 — Top-level integration driver for the per-epic e2e suite (J3).
#
# Runs `scripts/e2e_overhaul/<epic>.sh` in declared order, composes their J1
# log files into a single summary JSON, and exits 0 iff every per-epic
# script succeeded.
#
# Usage:
#   scripts/e2e_overhaul.sh                          # full run
#   scripts/e2e_overhaul.sh --only A,B               # only Epic A + B
#   scripts/e2e_overhaul.sh --skip H                 # skip Epic H
#   scripts/e2e_overhaul.sh --bail-on-first-failure  # stop at first epic failure
#   scripts/e2e_overhaul.sh --json-summary-to <path> # also write summary here
#
# Env:
#   EE_BINARY            path to ee binary (default: target/release/ee)
#   VERIFY_OVERHAUL      if "0" the driver is a no-op (used by verify.sh to
#                        gate the suite while implementation beads are still
#                        in flight; default: "1")
#
# Exit codes:
#   0  every selected epic passed
#   1  usage error
#   2  preflight error (missing binary, jq, etc.)
#   3  one or more epics failed
#
# Logs:
#   tests/logs/overhaul_<ISO>/summary.json    aggregate summary (J4 schema)
#   tests/logs/overhaul_<ISO>/<epic>.jsonl    per-epic J1 event stream (one
#                                             file per executed epic)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EPIC_DIR="$SCRIPT_DIR/e2e_overhaul"

DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"

if [ -d "${DEFAULT_AGENT_BUILD_ROOT}" ]; then
    mkdir -p "${DEFAULT_AGENT_BUILD_ROOT}/cargo-target" "${DEFAULT_AGENT_BUILD_ROOT}/tmp" 2>/dev/null || true
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${DEFAULT_AGENT_BUILD_ROOT}/cargo-target}"
    export TMPDIR="${EE_AGENT_TMPDIR:-${DEFAULT_AGENT_BUILD_ROOT}/tmp}"
fi

if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    EE_BINARY="${EE_BINARY:-${CARGO_TARGET_DIR%/}/release/ee}"
else
    EE_BINARY="${EE_BINARY:-$REPO_ROOT/target/release/ee}"
fi

# ---------------------------------------------------------------------------
# Epic registry. Letter → script-basename. Iterate in this order regardless
# of --only/--skip; that way the summary is deterministic across invocations.
# ---------------------------------------------------------------------------
EPIC_LETTERS=(A B C D E F G H I J K L)
declare -A EPIC_SCRIPTS=(
    [A]="pack_format.sh"
    [B]="search_honesty.sh"
    [C]="policy_detectors.sh"
    [D]="schema_consistency.sh"
    [E]="diagnostics_honesty.sh"
    [F]="discoverability.sh"
    [G]="learn_curate.sh"
    [H]="output_rendering.sh"
    [I]="agent_triad.sh"
    [J]="determinism.sh"
    [K]="verification_evidence.sh"
    [L]="failure_modes.sh"
)
declare -A EPIC_NAMES=(
    [A]="pack_format"
    [B]="search_honesty"
    [C]="policy_detectors"
    [D]="schema_consistency"
    [E]="diagnostics_honesty"
    [F]="discoverability"
    [G]="learn_curate"
    [H]="output_rendering"
    [I]="agent_triad"
    [J]="determinism"
    [K]="verification_evidence"
    [L]="failure_modes"
)

# ---------------------------------------------------------------------------
# Argument parsing.
# ---------------------------------------------------------------------------
ONLY=""
SKIP=""
BAIL=0
EXTRA_SUMMARY=""

usage() {
    cat <<'EOF'
Usage: scripts/e2e_overhaul.sh [options]

Options:
  --only <letters>            Run only the listed epics (comma-separated, e.g. "A,B").
  --skip <letters>            Skip the listed epics (comma-separated).
  --bail-on-first-failure     Stop after the first failing epic.
  --json-summary-to <path>    Copy the final summary JSON to <path>.
  -h, --help                  Show this help.

Env:
  EE_BINARY        Path to the ee binary (default: target/release/ee).
  VERIFY_OVERHAUL  Set to 0 to make this script a no-op (used by verify.sh).
EOF
}

while [ $# -gt 0 ]; do
    case "${1:-}" in
        --only)
            ONLY="${2:?--only requires an argument}"
            shift 2
            ;;
        --skip)
            SKIP="${2:?--skip requires an argument}"
            shift 2
            ;;
        --bail-on-first-failure)
            BAIL=1
            shift
            ;;
        --json-summary-to)
            EXTRA_SUMMARY="${2:?--json-summary-to requires an argument}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "j4: unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Quick no-op path so verify.sh can gate this suite while implementation
# beads are still in flight.
# ---------------------------------------------------------------------------
if [ "${VERIFY_OVERHAUL:-1}" = "0" ]; then
    echo "j4: VERIFY_OVERHAUL=0 — overhaul suite is gated off; exiting 0 without running." >&2
    exit 0
fi

# ---------------------------------------------------------------------------
# Preflight.
# ---------------------------------------------------------------------------
if [ ! -x "$EE_BINARY" ]; then
    echo "j4: ee binary not executable at $EE_BINARY" >&2
    echo "    set EE_BINARY or run: cargo build --release" >&2
    exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "j4: jq is required but was not found in PATH" >&2
    exit 2
fi
if ! command -v python3 >/dev/null 2>&1; then
    echo "j4: python3 is required for ISO timestamp + JSON aggregation" >&2
    exit 2
fi

ISO_RUN_ID="$(python3 -c 'from datetime import datetime, timezone; print(datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ"))')"
LOG_DIR="$REPO_ROOT/tests/logs/overhaul_${ISO_RUN_ID}"
mkdir -p "$LOG_DIR"

SUMMARY_PATH="$LOG_DIR/summary.json"

# ---------------------------------------------------------------------------
# Filter epics.
# ---------------------------------------------------------------------------
in_list() {
    # in_list <letter> <comma-list>
    local letter="$1"
    local list="${2:-}"
    IFS=',' read -ra parts <<< "$list"
    for p in "${parts[@]}"; do
        if [ "$(echo "$p" | tr '[:lower:]' '[:upper:]')" = "$letter" ]; then
            return 0
        fi
    done
    return 1
}

selected=()
for letter in "${EPIC_LETTERS[@]}"; do
    if [ -n "$ONLY" ]; then
        if ! in_list "$letter" "$ONLY"; then
            continue
        fi
    fi
    if [ -n "$SKIP" ] && in_list "$letter" "$SKIP"; then
        continue
    fi
    selected+=("$letter")
done

if [ ${#selected[@]} -eq 0 ]; then
    echo "j4: no epics selected after applying --only/--skip filters" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Compute the binary hash up front (matches J1's blake3 convention).
# ---------------------------------------------------------------------------
binary_hash() {
    if command -v b3sum >/dev/null 2>&1; then
        printf 'blake3:%s' "$(b3sum "$EE_BINARY" | awk '{print $1}')"
    elif python3 -c "import blake3" >/dev/null 2>&1; then
        python3 -c "import sys,blake3; print('blake3:'+blake3.blake3(open(sys.argv[1],'rb').read()).hexdigest())" "$EE_BINARY"
    else
        printf 'sha256:%s' "$(shasum -a 256 "$EE_BINARY" | awk '{print $1}')"
    fi
}
EE_BINARY_HASH="$(binary_hash)"
EE_VERSION="$("$EE_BINARY" --version 2>/dev/null | awk '{print $NF}' | head -1)"
EE_VERSION="${EE_VERSION:-unknown}"

STARTED_AT="$(python3 -c "from datetime import datetime, timezone; print(datetime.now(timezone.utc).isoformat(timespec='microseconds').replace('+00:00','Z'))")"

# ---------------------------------------------------------------------------
# Run each selected epic, capture status + assertion counts.
# ---------------------------------------------------------------------------
epic_records_file="$(mktemp)"
trap 'rm -f "$epic_records_file"'  EXIT

epics_pass=0
epics_fail=0
asserts_pass_total=0
asserts_fail_total=0
overall_exit=0

for letter in "${selected[@]}"; do
    script="${EPIC_SCRIPTS[$letter]}"
    epic_name="${EPIC_NAMES[$letter]}"
    epic_log="$LOG_DIR/${epic_name}.jsonl"
    epic_path="$EPIC_DIR/$script"

    if [ ! -x "$epic_path" ]; then
        echo "j4: missing executable epic script: $epic_path" >&2
        # Record a synthetic failure and continue (or bail).
        python3 - "$epic_records_file" "$letter" "$epic_name" \
            "missing_script" 0 0 0 "$epic_log" <<'PY'
import json, sys
out, letter, name, status, pass_n, fail_n, elapsed, log = sys.argv[1:]
record = {
    "letter": letter,
    "name": name,
    "status": status,
    "asserts": {"pass": int(pass_n), "fail": int(fail_n)},
    "elapsed_ms": int(elapsed),
    "log_path": log,
}
with open(out, "a") as f:
    f.write(json.dumps(record) + "\n")
PY
        epics_fail=$((epics_fail + 1))
        overall_exit=3
        if [ "$BAIL" -eq 1 ]; then
            break
        fi
        continue
    fi

    echo "[$letter] $epic_name → $(date -u +%H:%M:%S)" >&2

    epic_start_ns="$(python3 -c "from time import time_ns; print(time_ns())")"
    EE_TEST_LOG_PATH="$epic_log" \
        EE_BINARY="$EE_BINARY" \
        "$epic_path"
    rc=$?
    epic_end_ns="$(python3 -c "from time import time_ns; print(time_ns())")"
    elapsed_ms=$(( (epic_end_ns - epic_start_ns) / 1000000 ))

    # Pull pass/fail counts from the final J1 "test_end" event.
    epic_pass=0
    epic_fail=0
    if [ -s "$epic_log" ]; then
        counts="$(python3 - "$epic_log" <<'PY'
import json, sys
pp, ff = 0, 0
with open(sys.argv[1]) as f:
    for line in f:
        try:
            ev = json.loads(line)
        except Exception:
            continue
        if ev.get("kind") == "note" and "asserts_pass" in (ev.get("fields") or {}):
            pp = int(ev["fields"]["asserts_pass"])
            ff = int(ev["fields"]["asserts_fail"])
print(f"{pp} {ff}")
PY
)"
        epic_pass="${counts% *}"
        epic_fail="${counts#* }"
    fi

    asserts_pass_total=$((asserts_pass_total + epic_pass))
    asserts_fail_total=$((asserts_fail_total + epic_fail))

    if [ "$rc" -eq 0 ] && [ "$epic_fail" -eq 0 ]; then
        status="pass"
        epics_pass=$((epics_pass + 1))
    else
        # `rc != 0 && epic_fail == 0` means the epic script crashed mid-run
        # before emitting its final summary note. We still report the assertion
        # counts we have, but flag it as `error` rather than the cleaner `fail`.
        if [ "$rc" -ne 0 ] && [ "$epic_fail" -eq 0 ]; then
            status="error"
        else
            status="fail"
        fi
        epics_fail=$((epics_fail + 1))
        overall_exit=3
    fi

    python3 - "$epic_records_file" "$letter" "$epic_name" "$status" \
        "$epic_pass" "$epic_fail" "$elapsed_ms" "$epic_log" <<'PY'
import json, sys
out, letter, name, status, pass_n, fail_n, elapsed, log = sys.argv[1:]
record = {
    "letter": letter,
    "name": name,
    "status": status,
    "asserts": {"pass": int(pass_n), "fail": int(fail_n)},
    "elapsed_ms": int(elapsed),
    "log_path": log,
}
with open(out, "a") as f:
    f.write(json.dumps(record) + "\n")
PY

    if [ "$BAIL" -eq 1 ] && [ "$status" != "pass" ]; then
        echo "j4: --bail-on-first-failure: stopping after $epic_name ($status)" >&2
        break
    fi
done

ENDED_AT="$(python3 -c "from datetime import datetime, timezone; print(datetime.now(timezone.utc).isoformat(timespec='microseconds').replace('+00:00','Z'))")"

# ---------------------------------------------------------------------------
# Compose final summary JSON.
# ---------------------------------------------------------------------------
python3 - "$epic_records_file" "$SUMMARY_PATH" \
    "$STARTED_AT" "$ENDED_AT" "$EE_VERSION" "$EE_BINARY_HASH" \
    "$asserts_pass_total" "$asserts_fail_total" "$epics_pass" "$epics_fail" <<'PY'
import json, sys
records_path, summary_path, started, ended, ver, bhash, ap, af, ep, ef = sys.argv[1:]
epics = []
with open(records_path) as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        epics.append(json.loads(line))
summary = {
    "schema": "ee.e2e.overhaul.summary.v1",
    "started_at": started,
    "ended_at": ended,
    "ee_version": ver,
    "ee_binary_hash": bhash,
    "epics": epics,
    "totals": {
        "asserts_pass": int(ap),
        "asserts_fail": int(af),
        "epics_pass": int(ep),
        "epics_fail": int(ef),
    },
}
with open(summary_path, "w") as f:
    json.dump(summary, f, indent=2)
    f.write("\n")
PY

if [ -n "$EXTRA_SUMMARY" ]; then
    cp -- "$SUMMARY_PATH" "$EXTRA_SUMMARY"
fi

# ---------------------------------------------------------------------------
# Console summary so devs see results without inspecting JSON.
# ---------------------------------------------------------------------------
echo "j4: overhaul summary written to $SUMMARY_PATH"
python3 - "$SUMMARY_PATH" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
print()
print(f"  Overhaul e2e run: {data['started_at']} → {data['ended_at']}")
print(f"  ee version: {data['ee_version']}  binary: {data['ee_binary_hash']}")
print()
for epic in data["epics"]:
    print(f"  [{epic['letter']}] {epic['name']:<22} "
          f"{epic['status']:<5}  "
          f"pass={epic['asserts']['pass']:<3} "
          f"fail={epic['asserts']['fail']:<3} "
          f"({epic['elapsed_ms']} ms)")
t = data["totals"]
print()
print(f"  totals: epics_pass={t['epics_pass']} epics_fail={t['epics_fail']} "
      f"asserts_pass={t['asserts_pass']} asserts_fail={t['asserts_fail']}")
PY

exit "$overall_exit"
