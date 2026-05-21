#!/usr/bin/env bash
# bd-1zb7k.15.5 — no-mock verification reuse e2e driver.
#
# This script feeds synthetic J1 logs through the real `ee verify broker`
# importer and renders a real closeout capsule. It never launches Cargo.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "verification_reuse"

J1_LOG="$EPIC_WORKSPACE/verification_reuse_j1.jsonl"
SUMMARY_JSON="$EPIC_WORKSPACE/verification_reuse_summary.json"
DETERMINISM_A="$EPIC_WORKSPACE/verification_reuse_determinism_a.json"
DETERMINISM_B="$EPIC_WORKSPACE/verification_reuse_determinism_b.json"
DETERMINISM_C="$EPIC_WORKSPACE/verification_reuse_determinism_c.json"

cat > "$J1_LOG" <<'JSONL'
{"schema":"ee.test_event.v1","ts":"2026-05-21T18:00:00Z","test_id":"focused_rch_a","kind":"command_end","command":"cargo","args":["cargo","test","--test","verification_evidence_schema_unit","reuse_advisory","--","--nocapture"],"stdout_hash":"blake3:stdout-focused-a","stderr_excerpt":"remote worker css passed","exit_code":0,"elapsed_ms":42000.0}
{"schema":"ee.test_event.v1","ts":"2026-05-21T18:00:01Z","test_id":"focused_rch_a","kind":"artifact_manifest","fields":{"manifest_schema":"ee.test_artifact_manifest.v1","phase":"command_end","bead_id":"bd-1zb7k.15.5","agent_name":"CalmBay","source_hash":"blake3:source-current","command_hash":"blake3:focused-command","command_arg_count":"7","execution_substrate":"rch","worker_host":"css","target_directory":"/Volumes/USBNVME16TB/temp_agent_space/rch-target-focused-a","log_path":"/tmp/verification-reuse-focused-a.jsonl","artifact_manifest_hash":"blake3:manifest-focused-a"}}
{"schema":"ee.test_event.v1","ts":"2026-05-21T18:01:00Z","test_id":"focused_rch_b","kind":"command_end","command":"cargo","args":["cargo","test","--test","verification_evidence_schema_unit","reuse_advisory","--","--nocapture"],"stdout_hash":"blake3:stdout-focused-b","stderr_excerpt":"remote worker csd passed","exit_code":0,"elapsed_ms":39000.0}
{"schema":"ee.test_event.v1","ts":"2026-05-21T18:01:01Z","test_id":"focused_rch_b","kind":"artifact_manifest","fields":{"manifest_schema":"ee.test_artifact_manifest.v1","phase":"command_end","bead_id":"bd-1zb7k.15.5","agent_name":"RoseHill","source_hash":"blake3:source-current","command_hash":"blake3:focused-command","command_arg_count":"7","execution_substrate":"rch","worker_host":"csd","target_directory":"/Volumes/USBNVME16TB/temp_agent_space/rch-target-focused-b","log_path":"/tmp/verification-reuse-focused-b.jsonl","artifact_manifest_hash":"blake3:manifest-focused-b"}}
{"schema":"ee.test_event.v1","ts":"2026-05-21T17:45:00Z","test_id":"focused_rch_stale","kind":"command_end","command":"cargo","args":["cargo","test","--test","verification_evidence_schema_unit","reuse_advisory","--","--nocapture"],"stdout_hash":"blake3:stdout-stale","stderr_excerpt":"remote worker css passed on old source","exit_code":0,"elapsed_ms":41000.0}
{"schema":"ee.test_event.v1","ts":"2026-05-21T17:45:01Z","test_id":"focused_rch_stale","kind":"artifact_manifest","fields":{"manifest_schema":"ee.test_artifact_manifest.v1","phase":"command_end","bead_id":"bd-1zb7k.15.5","agent_name":"RubyWolf","source_hash":"blake3:source-stale","command_hash":"blake3:focused-command","command_arg_count":"7","execution_substrate":"rch","worker_host":"css","target_directory":"/Volumes/USBNVME16TB/temp_agent_space/rch-target-stale","log_path":"/tmp/verification-reuse-stale.jsonl","artifact_manifest_hash":"blake3:manifest-stale"}}
{"schema":"ee.test_event.v1","ts":"2026-05-21T18:02:00Z","test_id":"focused_rch_inflight","kind":"command_end","command":"cargo","args":["cargo","test","--test","verification_evidence_schema_unit","closeout_capsule","--","--nocapture"],"stdout_hash":"blake3:stdout-inflight","stderr_excerpt":"remote worker cse still running","elapsed_ms":0.0}
{"schema":"ee.test_event.v1","ts":"2026-05-21T18:02:01Z","test_id":"focused_rch_inflight","kind":"artifact_manifest","fields":{"manifest_schema":"ee.test_artifact_manifest.v1","phase":"command_end","bead_id":"bd-1zb7k.15.5","agent_name":"LilacRidge","source_hash":"blake3:source-current","command_hash":"blake3:inflight-command","command_arg_count":"7","execution_substrate":"rch","worker_host":"cse","target_directory":"/Volumes/USBNVME16TB/temp_agent_space/rch-target-inflight","log_path":"/tmp/verification-reuse-inflight.jsonl","artifact_manifest_hash":"blake3:manifest-inflight"}}
JSONL

focused_json=$(ee_workspace verify broker lookup \
    --runs-jsonl "$J1_LOG" \
    --source-hash blake3:source-current \
    --command-hash blake3:focused-command \
    --normalized-argv-hash blake3:focused-argv \
    --command-class cargo_test \
    --execution-substrate rch \
    --bead-id bd-1zb7k.15.5 \
    --json 2>/dev/null || true)
assert_jq "$focused_json" '.data.broker.status // empty' "reusable" \
    "verification_reuse_matching_focused_reusable"
assert_jq "$focused_json" '.data.broker.suggestedAction // empty' "cite_existing_run" \
    "verification_reuse_matching_focused_action"
assert_jq "$focused_json" '.data.evidenceCount // empty' "4" \
    "verification_reuse_imported_four_records"
matched_run_id="$(printf '%s' "$focused_json" | jq -r '.data.broker.matchedRunId // empty')"
assert_jq_nonempty "$focused_json" '.data.broker.matchedRunId // empty' \
    "verification_reuse_matched_run_id"

inflight_json=$(ee_workspace verify broker lookup \
    --runs-jsonl "$J1_LOG" \
    --source-hash blake3:source-current \
    --command-hash blake3:inflight-command \
    --normalized-argv-hash blake3:inflight-argv \
    --command-class cargo_test \
    --execution-substrate rch \
    --bead-id bd-1zb7k.15.5 \
    --json 2>/dev/null || true)
assert_jq "$inflight_json" '.data.broker.status // empty' "in_progress" \
    "verification_reuse_inflight_waits"
assert_jq "$inflight_json" '.data.broker.suggestedAction // empty' "wait_for_in_progress_run" \
    "verification_reuse_inflight_action"

mismatch_json=$(ee_workspace verify broker lookup \
    --runs-jsonl "$J1_LOG" \
    --source-hash blake3:source-current \
    --command-hash blake3:broad-workspace-command \
    --normalized-argv-hash blake3:broad-workspace-argv \
    --command-class cargo_test \
    --execution-substrate rch \
    --bead-id bd-1zb7k.15.5 \
    --json 2>/dev/null || true)
assert_jq "$mismatch_json" '.data.broker.status // empty' "incompatible" \
    "verification_reuse_broad_command_mismatch"
assert_jq "$mismatch_json" '(.data.broker.staleReasonCodes | index("command_hash_mismatch") != null)' "true" \
    "verification_reuse_broad_command_reason"

capsule_json=$(ee_workspace verify closeout capsule \
    --runs-jsonl "$J1_LOG" \
    --run-id "$matched_run_id" \
    --bead-id bd-1zb7k.15.5 \
    --source-hash blake3:source-current \
    --reusable-until 2026-05-21T19:01:00Z \
    --json 2>/dev/null || true)
assert_jq "$capsule_json" '.data.closeoutCapsule.schema // empty' \
    "ee.verification.closeout_capsule.v1" \
    "verification_reuse_capsule_schema"
assert_jq "$capsule_json" '.data.closeoutCapsule.executionSubstrate // empty' "rch" \
    "verification_reuse_capsule_substrate"
assert_jq_nonempty "$capsule_json" '.data.closeoutCapsule.workerHost // empty' \
    "verification_reuse_capsule_worker"
assert_jq "$capsule_json" '.data.closeoutCapsule.supportBundleMetadata.rawOutputIncluded // true' "false" \
    "verification_reuse_capsule_no_raw_output"
assert_jq "$capsule_json" '.data.closeoutCapsule.supportBundleMetadata.localPathsRedacted // false' "true" \
    "verification_reuse_capsule_redacts_paths"
if printf '%s' "$capsule_json" | grep -E '/Volumes/USBNVME16TB|/tmp/|stderr bytes|remote worker' >/dev/null; then
    e2e_log_assert_eq "raw output or path leak" "none" \
        "verification_reuse_capsule_redaction_scan"
else
    e2e_log_assert_eq "none" "none" "verification_reuse_capsule_redaction_scan"
fi

normalize_for_determinism() {
    jq -S '{broker: .data.broker, evidenceCount: .data.evidenceCount, source: .data.source}' \
        > "$1"
}

printf '%s' "$focused_json" | normalize_for_determinism "$DETERMINISM_A"
ee_workspace verify broker lookup \
    --runs-jsonl "$J1_LOG" \
    --source-hash blake3:source-current \
    --command-hash blake3:focused-command \
    --normalized-argv-hash blake3:focused-argv \
    --command-class cargo_test \
    --execution-substrate rch \
    --bead-id bd-1zb7k.15.5 \
    --json 2>/dev/null | normalize_for_determinism "$DETERMINISM_B"
ee_workspace verify broker lookup \
    --runs-jsonl "$J1_LOG" \
    --source-hash blake3:source-current \
    --command-hash blake3:focused-command \
    --normalized-argv-hash blake3:focused-argv \
    --command-class cargo_test \
    --execution-substrate rch \
    --bead-id bd-1zb7k.15.5 \
    --json 2>/dev/null | normalize_for_determinism "$DETERMINISM_C"

if cmp -s "$DETERMINISM_A" "$DETERMINISM_B" && cmp -s "$DETERMINISM_A" "$DETERMINISM_C"; then
    e2e_log_assert_eq "deterministic" "deterministic" \
        "verification_reuse_three_run_determinism"
else
    e2e_log_assert_eq "deterministic" "drifted" \
        "verification_reuse_three_run_determinism"
fi

retained_log_value="null"
if [ "${EE_E2E_KEEP_ARTIFACTS:-0}" = "1" ]; then
    retained_log_value="$(printf '%s' "$J1_LOG" | jq -R .)"
fi

jq -n \
    --arg schema "ee.e2e.verification_reuse.v1" \
    --arg beadId "bd-1zb7k.15.5" \
    --arg focusedStatus "$(printf '%s' "$focused_json" | jq -r '.data.broker.status // empty')" \
    --arg inflightStatus "$(printf '%s' "$inflight_json" | jq -r '.data.broker.status // empty')" \
    --arg mismatchStatus "$(printf '%s' "$mismatch_json" | jq -r '.data.broker.status // empty')" \
    --arg capsuleResult "$(printf '%s' "$capsule_json" | jq -r '.data.closeoutCapsule.result // empty')" \
    --argjson retainedLogPath "$retained_log_value" \
    '{
        schema: $schema,
        beadId: $beadId,
        focusedStatus: $focusedStatus,
        inflightStatus: $inflightStatus,
        mismatchStatus: $mismatchStatus,
        closeoutCapsuleResult: $capsuleResult,
        cargoExecuted: false,
        retainedLogPath: $retainedLogPath,
        remediationSurfaces: ["bd-1zb7k.15.1", "bd-1zb7k.15.2", "bd-1zb7k.15.3", "bd-1zb7k.15.4"]
    }' > "$SUMMARY_JSON"

SUMMARY_TEXT="$(cat "$SUMMARY_JSON")"
assert_jq "$SUMMARY_TEXT" '.schema // empty' "ee.e2e.verification_reuse.v1" \
    "verification_reuse_summary_schema"
assert_jq "$SUMMARY_TEXT" '.cargoExecuted // true' "false" \
    "verification_reuse_summary_no_cargo"
e2e_log_note "verification_reuse_summary path=$SUMMARY_JSON"
