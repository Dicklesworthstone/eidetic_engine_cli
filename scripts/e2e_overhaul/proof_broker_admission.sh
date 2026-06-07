#!/usr/bin/env bash
# bd-1n3x1.6 — no-mock proof-broker public surface e2e driver.
#
# This script exercises the real `ee proof admit` and `ee proof status`
# surfaces against committed proof-broker ledger fixtures. It never launches
# Cargo or RCH; admission remains read-only and verifies whether a caller should
# reuse, wait, reject stale source state, or dispatch one remote proof.
#
# RCH-only proof recipe for this bead, when a current ee binary is available:
#
#   RCH_VERIFY_ATTEMPT_TIMEOUT_MS=1800000 \
#   RCH_REQUIRE_REMOTE=1 \
#   RCH_VISIBILITY=summary \
#   RCH_VERIFY_TAIL_BYTES=12000 \
#   TMPDIR=/Volumes/USBNVME16TB/temp_agent_space/tmp \
#   scripts/rch_verify.sh --skip-known-blocker --env RUSTFLAGS=-Awarnings -- \
#     cargo test --test rch_verify_contract \
#       proof_broker_environment_blocked_refuses_before_remote_dispatch \
#       -- --exact --nocapture
#
# Expected artifacts are reported by the ee.rch.verify.v1 `artifacts[]` block,
# typically retained stdout/stderr files under /tmp/rch-verify-primary-*. A
# pre-Cargo RCH-E327/no-worker/capacity timeout is environment evidence, not a
# source failure; preserve the exact `status`, `degraded_codes`, and
# `selector_admission_probe.selection_failure_reason`. Do not rerun while RCH
# still lists an equivalent active build, and do not increase fanout just to
# force duplicate dispatch.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "proof_broker_admission"

LEDGER_FIXTURE="$REPO_ROOT/tests/fixtures/golden/verification/proof_broker_records.json.golden"
LEDGER_JSON="$EPIC_WORKSPACE/proof_broker_records.json"
SUMMARY_JSON="$EPIC_WORKSPACE/proof_broker_admission_summary.json"

if [ ! -f "$LEDGER_FIXTURE" ]; then
    e2e_log_assert_eq "present" "missing" "proof_broker_ledger_fixture_exists"
    e2e_log_note "missing proof broker ledger fixture path=$LEDGER_FIXTURE"
    exit 3
fi

cp "$LEDGER_FIXTURE" "$LEDGER_JSON"

now_ms() {
    python3 - <<'PY'
import time
print(time.monotonic_ns())
PY
}

elapsed_ms_since() {
    local started="${1:?started ns required}"
    python3 - "$started" <<'PY'
import sys
import time
print((time.monotonic_ns() - int(sys.argv[1])) / 1_000_000.0)
PY
}

emit_broker_event() {
    local label="${1:?label required}"
    local command_text="${2:?command text required}"
    local stdout_path="${3:?stdout path required}"
    local stderr_path="${4:?stderr path required}"
    local exit_code="${5:?exit code required}"
    local elapsed_ms="${6:?elapsed ms required}"
    local expected_verdict="${7:?expected verdict required}"
    local stdout_hash stderr_hash
    stdout_hash="$(_e2e_hash_file "$stdout_path")"
    stderr_hash="$(_e2e_hash_file "$stderr_path")"

    python3 - "$EE_TEST_LOG_PATH" "$EE_TEST_LOG_TEST_ID" "$label" "$command_text" \
        "$stdout_path" "$stderr_path" "$exit_code" "$elapsed_ms" "$expected_verdict" \
        "$EPIC_WORKSPACE" "$PWD" "$stdout_hash" "$stderr_hash" <<'PY'
import json
import os
import re
import sys
from datetime import datetime, timezone

(
    log_path,
    test_id,
    label,
    command_text,
    stdout_path,
    stderr_path,
    exit_code,
    elapsed_ms,
    expected_verdict,
    workspace,
    cwd,
    stdout_hash,
    stderr_hash,
) = sys.argv[1:]

try:
    with open(stdout_path, encoding="utf-8") as handle:
        stdout_text = handle.read()
except OSError:
    stdout_text = ""
try:
    with open(stderr_path, encoding="utf-8") as handle:
        stderr_text = handle.read()
except OSError:
    stderr_text = ""

try:
    payload = json.loads(stdout_text)
except json.JSONDecodeError:
    payload = {}

data = payload.get("data") if isinstance(payload, dict) else {}
if not isinstance(data, dict):
    data = {}
admission = data.get("admission") or {}
if not isinstance(admission, dict):
    admission = {}
matched = data.get("matchedRecord") or {}
if not isinstance(matched, dict):
    matched = {}
fingerprint = data.get("fingerprint") or {}
if not isinstance(fingerprint, dict):
    fingerprint = {}
proof_fingerprint = (
    fingerprint.get("fingerprintId")
    or data.get("fingerprintId")
    or ""
)
evidence_refs = matched.get("evidenceRefs") or []
if not isinstance(evidence_refs, list):
    evidence_refs = []
first_evidence = evidence_refs[0] if evidence_refs and isinstance(evidence_refs[0], dict) else {}
broker_verdict = admission.get("verdict") or ""
owner_status = data.get("ownerStatus") or {}
if not isinstance(owner_status, dict):
    owner_status = {}
owner = owner_status.get("owner") or {}
if not isinstance(owner, dict):
    owner = {}

schema_ok = (
    payload.get("schema") == "ee.response.v2"
    and isinstance(data, dict)
    and data.get("schema") == "ee.proof_broker.v1"
)
leak_pattern = re.compile(r"(?i)/Users/jemanuel|(token|secret|password|api[_-]?key)=")
redaction_ok = not leak_pattern.search(stdout_text + "\n" + stderr_text)
stderr_empty = len(stderr_text) == 0
exit_ok = int(exit_code) == 0
verdict_ok = broker_verdict == expected_verdict
diagnosis = "none" if schema_ok and redaction_ok and stderr_empty and exit_ok and verdict_ok else "proof_broker_public_surface_mismatch"

event = {
    "schema": "ee.test_event.v1",
    "ts": datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "test_id": test_id,
    "kind": "assert_result",
    "command": command_text,
    "stdout_hash": stdout_hash,
    "stderr_hash": stderr_hash,
    "exit_code": int(exit_code),
    "elapsed_ms": float(elapsed_ms),
    "fields": {
        "label": label,
        "cwd": cwd,
        "workspace": workspace,
        "sanitized_env": {
            "HOME": "[HOME]",
            "CARGO_TARGET_DIR": "[CARGO_TARGET_DIR]",
            "TMPDIR": "[TMPDIR]",
            "EE_WORKSPACE": "[unset]",
        },
        "stdout_artifact_path": stdout_path,
        "stderr_artifact_path": stderr_path,
        "schema_validation_status": "passed" if schema_ok else "failed",
        "redaction_status": "passed" if redaction_ok else "failed",
        "broker_verdict": broker_verdict,
        "expected_broker_verdict": expected_verdict,
        "proof_fingerprint": proof_fingerprint,
        "reused_proof_id": admission.get("reuseRunId") or "",
        "reused_proof_hash": first_evidence.get("contentHash") or "",
        "wait_owner_job_id": (
            (admission.get("waitOwner") or {}).get("rchJobId")
            or owner.get("rchJobId")
            or ""
        ),
        "first_failure_diagnosis": diagnosis,
    },
}

os.makedirs(os.path.dirname(log_path) or ".", exist_ok=True)
with open(log_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(event, sort_keys=True) + "\n")
PY
}

run_proof_case() {
    local label="${1:?label required}"
    local expected_verdict="${2:?expected verdict required}"
    shift 2
    local stdout_path="$EPIC_WORKSPACE/${label}.stdout.json"
    local stderr_path="$EPIC_WORKSPACE/${label}.stderr.log"
    local started elapsed_ms exit_code
    local command_text="$EE_BINARY --workspace $EPIC_WORKSPACE --json $*"

    started="$(now_ms)"
    "$EE_BINARY" --workspace "$EPIC_WORKSPACE" --json "$@" >"$stdout_path" 2>"$stderr_path"
    exit_code=$?
    elapsed_ms="$(elapsed_ms_since "$started")"

    local response
    response="$(cat "$stdout_path")"
    assert_jq "$response" '.schema // empty' "ee.response.v2" "${label}_response_schema"
    assert_jq "$response" '.data.schema // empty' "ee.proof_broker.v1" "${label}_broker_schema"
    assert_jq "$response" '.data.admission.verdict // empty' "$expected_verdict" "${label}_verdict"
    e2e_log_assert_eq "$exit_code" "0" "${label}_exit_zero"
    e2e_log_assert_num "$(wc -c <"$stderr_path")" -eq 0 "${label}_stderr_empty"
    emit_broker_event "$label" "$command_text" "$stdout_path" "$stderr_path" \
        "$exit_code" "$elapsed_ms" "$expected_verdict"
}

COMMON_ADMIT_ARGS=(
    "proof" "admit"
    "--ledger-json" "$LEDGER_JSON"
    "--now" "2026-06-05T18:20:00Z"
    "--agent-mail-status" "live"
    "--bead-id" "bd-1n3x1.1"
    "--command-class" "cargo_test"
    "--source-materialization" "git_worktree"
    "--dirty-status-hash" "blake3:dirty-status-clean"
    "--env-fingerprint-class" "class:external_cargo_target"
    "--target-profile" "debug"
    "--execution-substrate" "rch"
    "--rch-runtime-class" "class:rch_client_1_0_37_daemon_0_1_3"
    "--worker-requirement" "required_runtime:rust"
    "--local-cargo-tripwire-class" "class:tripwire_clean"
    "--build-admission-posture" "remote_required_no_local_fallback"
)

run_admit_case() {
    local label="${1:?label required}"
    local expected_verdict="${2:?expected verdict required}"
    local command_hash="${3:?command hash required}"
    local normalized_argv_hash="${4:?normalized argv hash required}"
    local source_hash="${5:?source hash required}"
    local env_fingerprint_class="${6:?env fingerprint class required}"
    local rch_runtime_class="${7:?rch runtime class required}"
    local local_cargo_tripwire_class="${8:?local cargo tripwire class required}"
    local build_admission_posture="${9:?build admission posture required}"
    local test_filter="${10:?test filter required}"

    run_proof_case "$label" "$expected_verdict" \
        "proof" "admit" \
        "--ledger-json" "$LEDGER_JSON" \
        "--now" "2026-06-05T18:20:00Z" \
        "--agent-mail-status" "live" \
        "--bead-id" "bd-1n3x1.1" \
        "--command-class" "cargo_test" \
        "--source-materialization" "git_worktree" \
        "--dirty-status-hash" "blake3:dirty-status-clean" \
        "--target-profile" "debug" \
        "--execution-substrate" "rch" \
        "--worker-requirement" "required_runtime:rust" \
        "--command-hash" "$command_hash" \
        "--normalized-argv-hash" "$normalized_argv_hash" \
        "--source-hash" "$source_hash" \
        "--env-fingerprint-class" "$env_fingerprint_class" \
        "--rch-runtime-class" "$rch_runtime_class" \
        "--local-cargo-tripwire-class" "$local_cargo_tripwire_class" \
        "--build-admission-posture" "$build_admission_posture" \
        "--" "cargo" "test" "--test" "rch_verify_contract" "$test_filter"
}

run_proof_case "reuse_existing_admit" "reuse_existing" \
    "${COMMON_ADMIT_ARGS[@]}" \
    "--command-hash" "blake3:rch-command" \
    "--normalized-argv-hash" "blake3:rch-command-argv" \
    "--source-hash" "blake3:source" \
    "--" "cargo" "test" "--test" "rch_verify_contract" "proof_broker"

run_proof_case "wait_for_inflight_admit" "wait_for_inflight" \
    "${COMMON_ADMIT_ARGS[@]}" \
    "--command-hash" "blake3:in-flight-command" \
    "--normalized-argv-hash" "blake3:in-flight-argv" \
    "--source-hash" "blake3:source" \
    "--" "cargo" "test" "--test" "rch_verify_contract" "proof_broker_inflight"

run_proof_case "source_state_mismatch_admit" "source_state_mismatch" \
    "${COMMON_ADMIT_ARGS[@]}" \
    "--command-hash" "blake3:rch-command" \
    "--normalized-argv-hash" "blake3:rch-command-argv" \
    "--source-hash" "blake3:dirty-current-tree" \
    "--" "cargo" "test" "--test" "rch_verify_contract" "proof_broker_stale_source"

run_admit_case "dispatch_allowed_admit" "dispatch_allowed" \
    "blake3:new-command" \
    "blake3:new-argv" \
    "blake3:source-v2" \
    "class:external_cargo_target" \
    "class:rch_client_1_0_37_daemon_0_1_3" \
    "class:tripwire_clean" \
    "remote_required_no_local_fallback" \
    "proof_broker_dispatch_allowed"

run_admit_case "environment_blocked_admit" "environment_blocked" \
    "blake3:env-blocked-command" \
    "blake3:env-blocked-argv" \
    "blake3:source" \
    "class:external_cargo_target" \
    "class:rch_runtime_mismatch" \
    "class:tripwire_clean" \
    "remote_required_no_local_fallback" \
    "proof_broker_environment_blocked"

run_admit_case "proof_unusable_admit" "proof_unusable" \
    "blake3:local-cargo-command" \
    "blake3:local-cargo-argv" \
    "blake3:source" \
    "class:external_cargo_target" \
    "class:rch_client_1_0_37_daemon_0_1_3" \
    "class:local_cargo_bypass_detected" \
    "remote_required_no_local_fallback" \
    "proof_broker_local_cargo_bypass"

run_admit_case "unknown_insufficient_evidence_admit" "unknown_insufficient_evidence" \
    "blake3:ambiguous-command" \
    "blake3:ambiguous-argv" \
    "class:unknown_source" \
    "class:unknown_env" \
    "class:unknown_rch_runtime" \
    "class:tripwire_unknown" \
    "remote_required_no_local_fallback" \
    "proof_broker_unknown_evidence"

run_proof_case "wait_for_inflight_status" "wait_for_inflight" \
    "proof" "status" \
    "--ledger-json" "$LEDGER_JSON" \
    "--now" "2026-06-05T18:20:00Z" \
    "--agent-mail-status" "live" \
    "--fingerprint" "proof_ea208dc26b8228a65cbaae099a"

custom_event_count="$(jq -sr '[.[] | select(.schema == "ee.test_event.v1" and .kind == "assert_result" and (.fields.broker_verdict // "") != "")] | length' "$EE_TEST_LOG_PATH")"
e2e_log_assert_num "$custom_event_count" -ge 8 "proof_broker_custom_event_rows"

jq -n \
    --arg schema "ee.e2e.proof_broker_admission.v1" \
    --arg beadId "bd-1n3x1.6" \
    --arg ledger "$LEDGER_JSON" \
    --argjson customEventCount "$custom_event_count" \
    '{
        schema: $schema,
        beadId: $beadId,
        ledgerPath: $ledger,
        cargoExecuted: false,
        rchExecuted: false,
        publicSurfaces: ["ee proof admit", "ee proof status"],
        coveredVerdicts: [
            "reuse_existing",
            "wait_for_inflight",
            "source_state_mismatch",
            "dispatch_allowed",
            "environment_blocked",
            "proof_unusable",
            "unknown_insufficient_evidence"
        ],
        customEventCount: $customEventCount
    }' > "$SUMMARY_JSON"

SUMMARY_TEXT="$(cat "$SUMMARY_JSON")"
assert_jq "$SUMMARY_TEXT" '.schema // empty' "ee.e2e.proof_broker_admission.v1" \
    "proof_broker_summary_schema"
assert_jq "$SUMMARY_TEXT" '.cargoExecuted | tostring' "false" \
    "proof_broker_summary_no_cargo"
assert_jq "$SUMMARY_TEXT" '.rchExecuted | tostring' "false" \
    "proof_broker_summary_no_rch"
e2e_log_note "proof_broker_admission_summary path=$SUMMARY_JSON"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 3
fi
