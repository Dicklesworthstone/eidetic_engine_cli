#!/usr/bin/env bash
# bd-21joy — ee doctor safety harness gate.
#
# Stage 8 of scripts/verify.sh delegates to this wrapper. The wrapper
# runs the five sub-harnesses (verify-undo, verify-idempotence,
# verify-crash-recovery, verify-concurrency, verify-metamorphic) against
# the per-failure-mode fixture suite under tests/doctor_fixtures/.
#
# The fixture suite is owned by bd-2oh15 (per-FM fixture suite) which
# is in_progress at the time this wrapper landed. While
# tests/doctor_fixtures/ is missing or empty, this wrapper emits a
# stable `safety_harness_fixtures_unavailable` degraded event and exits
# 0 (advisory). Once bd-2oh15 lands the fixture suite, each sub-script
# can be filled in independently and this wrapper will start exercising
# them automatically.
#
# Acceptance contract (bd-21joy):
#   - Stage 8 of scripts/verify.sh invokes this wrapper.
#   - Wrapper runs scripts/verify-undo.sh, verify-idempotence.sh,
#     verify-crash-recovery.sh, verify-concurrency.sh,
#     verify-metamorphic.sh in order.
#   - Missing fixture suite or missing sub-script is reported as a
#     stable degraded code; never fails closed unless EE_SAFETY_HARNESS_STRICT=1.
#   - Emits one ee.test_event.v1 row per sub-harness plus a summary row.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# WHICH BINARY THE SUB-HARNESSES GRADE. All five read
# EE_DOCTOR_FIXTURE_BINARY and default it to bare `ee`, i.e. PATH. Until this
# block landed, NOTHING in the chain set that variable: scripts/verify.sh has
# zero references to it and so did this wrapper, so Gate 8.5 ran whatever ee
# happened to be installed, in every verify.sh run -- not only standalone.
#
# That mattered because verify.sh works hard to attest the binary it BUILT
# (EE_BINARY_SHA256 at verify.sh:428, target triple at :566, target OS at
# :624) and none of those facts described what these five actually executed.
# Measured on the Mac dev host 2026-10-13: PATH ee was 0.14.2 while
# Cargo.toml said 0.17.0, three minor versions apart.
#
# Note the asymmetry this repairs: a MISSING ee already failed the gate loudly
# (each sub-script exits 3, which this wrapper treats as a failure), while a
# WRONG ee passed it silently. The silent case is the one that needed fixing.
#
# Precedence: an explicit EE_DOCTOR_FIXTURE_BINARY still wins, so callers that
# deliberately pin a fixture binary are untouched. Otherwise take verify.sh's
# exported EE_BINARY -- the binary it built and attested -- and fall back to
# Cargo's effective target directory for a standalone run.
# shellcheck source=scripts/lib/ee_binary_resolution.sh
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/lib/ee_binary_resolution.sh"
EE_DOCTOR_FIXTURE_BINARY="${EE_DOCTOR_FIXTURE_BINARY:-${EE_BINARY:-$(ee_resolve_binary debug)}}"
export EE_DOCTOR_FIXTURE_BINARY
# Print it BEFORE any sub-harness runs, so a log reader can see which binary
# produced the verdicts even when they all pass.
printf 'safety harness: ee_binary=%s\n' "$EE_DOCTOR_FIXTURE_BINARY" >&2

FIXTURE_DIR="${EE_SAFETY_HARNESS_FIXTURE_DIR:-$REPO_ROOT/tests/doctor_fixtures}"
EVENT_LOG="${EE_SAFETY_HARNESS_EVENT_LOG:-}"
STRICT="${EE_SAFETY_HARNESS_STRICT:-0}"

SUB_HARNESSES=(
    "verify-undo.sh"
    "verify-idempotence.sh"
    "verify-crash-recovery.sh"
    "verify-concurrency.sh"
    "verify-metamorphic.sh"
)

emit_event() {
    local kind="$1"
    local phase="$2"
    local status="$3"
    local exit_code="$4"
    local degraded_code="${5:-}"
    local detail="${6:-}"

    if [ -z "$EVENT_LOG" ]; then
        return 0
    fi
    local ts
    ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    python3 - "$EVENT_LOG" "$ts" "$kind" "$phase" "$status" "$exit_code" "$degraded_code" "$detail" <<'PY'
import json
import sys

event_log, ts, kind, phase, status, exit_code, degraded_code, detail = sys.argv[1:9]
fields = {
    "bead_id": "bd-21joy",
    "surface": "safety_harness",
    "phase": phase,
}
if degraded_code:
    fields["degradedCode"] = degraded_code
if detail:
    fields["detail"] = detail

event = {
    "schema": "ee.test_event.v1",
    "ts": ts,
    "kind": kind,
    "status": status,
    "exitCode": int(exit_code),
    "elapsedMs": 0,
    "fields": fields,
}
with open(event_log, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(event, sort_keys=True) + "\n")
PY
}

abort_if_strict() {
    local degraded_code="$1"
    local detail="$2"
    if [ "$STRICT" = "1" ]; then
        echo "safety harness: $degraded_code: $detail" >&2
        emit_event "safety_harness_summary" "summary" "failed" "1" "$degraded_code" "$detail"
        exit 1
    fi
    echo "safety harness: $degraded_code (advisory): $detail" >&2
    emit_event "safety_harness_summary" "summary" "advisory" "0" "$degraded_code" "$detail"
    exit 0
}

emit_event "safety_harness_setup" "setup" "passed" "0" "" "fixture_dir=$FIXTURE_DIR"

# Binary usability gate. Resolving a path is NOT enough, and this gate is the
# proof: measured on the Mac dev host 2026-10-13, the resolved target-dir
# binary was a Linux x86-64 ELF left behind by an RCH run, and it satisfied
# [ -x ] while failing with `exec format error`. Running the five sub-harnesses
# against it would report twenty-odd assertion failures whose single real cause
# is the wrong platform -- the class scripts/lib/ee_binary_resolution.sh
# already documents, and that e2e_session_budget.sh hit for 15 assert_fails.
#
# ee_require_current_binary checks all three things one at a time: the file
# exists and is executable, it can actually EXECUTE on this host (126/127), and
# its version equals Cargo.toml. It prints the resolved path and both versions
# before deciding, so the log shows which binary produced a verdict.
#
# Routed through abort_if_strict rather than a hard exit, to match how this
# wrapper already treats environmental gaps (missing fixture dir, missing
# sub-script): advisory by default, fail-closed under
# EE_SAFETY_HARNESS_STRICT=1. The point is that the gate now says "I did not
# measure" instead of reporting a pass it did not earn.
if ! ee_require_current_binary "$EE_DOCTOR_FIXTURE_BINARY" "safety harness"; then
    abort_if_strict "safety_harness_binary_unusable" \
        "$EE_DOCTOR_FIXTURE_BINARY is missing, foreign to this host, or not the current source version"
fi

# Check the shared assertion oracle before trusting any per-FM result. These
# controls use a test double; they do not establish that real repairs work.
if python3 "$REPO_ROOT/tests/doctor_fixtures/assertion_contract.py"; then
    emit_event "safety_harness_sub" "assertion-contract" "passed" "0" "" ""
else
    echo "safety harness: doctor fixture assertion contract failed" >&2
    emit_event "safety_harness_sub" "assertion-contract" "failed" "1" "safety_harness_sub_failed" ""
    emit_event "safety_harness_summary" "summary" "failed" "1" "safety_harness_sub_failed" "assertion-contract"
    exit 1
fi

# Fixture suite gate. While bd-2oh15 is in_progress and the directory
# is missing or empty, this is the expected condition.
if [ ! -d "$FIXTURE_DIR" ]; then
    abort_if_strict "safety_harness_fixtures_unavailable" \
        "$FIXTURE_DIR does not exist (bd-2oh15 owns the per-FM fixture suite)"
fi
if [ -z "$(ls -A "$FIXTURE_DIR" 2>/dev/null || true)" ]; then
    abort_if_strict "safety_harness_fixtures_unavailable" \
        "$FIXTURE_DIR is empty (bd-2oh15 owns the per-FM fixture suite)"
fi

# Sub-harness gate. Each sub-script must exist; missing scripts are an
# advisory degradation while bd-21joy follow-up slices fill them in.
MISSING_SUB_HARNESSES=""
for sub in "${SUB_HARNESSES[@]}"; do
    if [ ! -x "$SCRIPT_DIR/$sub" ]; then
        MISSING_SUB_HARNESSES="$MISSING_SUB_HARNESSES $sub"
    fi
done
if [ -n "$MISSING_SUB_HARNESSES" ]; then
    abort_if_strict "safety_harness_sub_scripts_missing" \
        "missing sub-scripts:$MISSING_SUB_HARNESSES"
fi

# Run sub-harnesses. Each must exit 0; on first failure stop and emit
# the failing phase. The fixture-suite contract is owned by bd-2oh15.
FAILED_HARNESS=""
for sub in "${SUB_HARNESSES[@]}"; do
    phase="${sub%.sh}"
    emit_event "safety_harness_sub" "$phase" "running" "0" "" ""
    if ! "$SCRIPT_DIR/$sub"; then
        FAILED_HARNESS="$sub"
        emit_event "safety_harness_sub" "$phase" "failed" "1" "safety_harness_sub_failed" ""
        break
    fi
    emit_event "safety_harness_sub" "$phase" "passed" "0" "" ""
done

if [ -n "$FAILED_HARNESS" ]; then
    echo "safety harness: sub-harness $FAILED_HARNESS failed" >&2
    emit_event "safety_harness_summary" "summary" "failed" "1" "safety_harness_sub_failed" "$FAILED_HARNESS"
    exit 1
fi

emit_event "safety_harness_summary" "summary" "passed" "0" "" ""
echo "safety harness: all sub-harnesses passed against $FIXTURE_DIR"
exit 0
