#!/usr/bin/env bash
# J3 — S2 verification evidence ledger e2e driver.
#
# Asserts the durable verification ingestion path, why attachment, idempotent
# replay signal, and remote-required fallback closure rejection.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "s2_verification_evidence"
seed_corpus

GOLDEN_EVIDENCE="$REPO_ROOT/tests/fixtures/golden/models/verification_evidence_records.json.golden"
if [ ! -f "$GOLDEN_EVIDENCE" ]; then
    e2e_log_assert_eq "missing" "$GOLDEN_EVIDENCE" "verification_evidence_fixture_exists"
    exit 0
fi

MEMORY_JSON=$(ee_workspace remember \
    "S2 verification evidence e2e target memory." \
    --level procedural \
    --kind rule \
    --no-propose-candidates \
    --json 2>/dev/null || true)
MEMORY_ID=$(printf '%s' "$MEMORY_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
assert_jq_nonempty "$MEMORY_JSON" '.data.memory_id // empty' "verification_target_memory_created"

if [ -z "$MEMORY_ID" ]; then
    e2e_log_note "verification_evidence_skip_no_memory_id"
    exit 0
fi

PASS_EVIDENCE=$(jq -c '.[0]' "$GOLDEN_EVIDENCE")
INGEST_JSON=$(printf '%s' "$PASS_EVIDENCE" \
    | ee_workspace verification ingest \
        --stdin \
        --target-type memory \
        --target-id "$MEMORY_ID" \
        --actor "verification_evidence_e2e" \
        --json 2>/dev/null || true)
assert_jq "$INGEST_JSON" '.data.command // empty' "verification ingest" \
    "verification_ingest_command"
assert_jq "$INGEST_JSON" '.data.persisted // false' "true" \
    "verification_ingest_persisted"
assert_jq_nonempty "$INGEST_JSON" '.data.auditId // empty' \
    "verification_ingest_audit_id"
assert_jq_nonempty "$INGEST_JSON" '.data.contentHash // empty' \
    "verification_ingest_content_hash"

WHY_JSON=$(ee_workspace why "$MEMORY_ID" --json 2>/dev/null || true)
WHY_EVIDENCE_COUNT=$(printf '%s' "$WHY_JSON" \
    | jq '.data.verificationEvidence | length' 2>/dev/null || echo 0)
e2e_log_assert_num "$WHY_EVIDENCE_COUNT" -ge 1 "why_attaches_verification_evidence"
assert_jq "$WHY_JSON" '.data.verificationEvidence[0].verificationId // empty' \
    "ver_pass_00000000000000000001" \
    "why_verification_evidence_id"

REPLAY_JSON=$(printf '%s' "$PASS_EVIDENCE" \
    | ee_workspace verification ingest \
        --stdin \
        --target-type memory \
        --target-id "$MEMORY_ID" \
        --actor "verification_evidence_e2e" \
        --json 2>/dev/null || true)
assert_jq "$REPLAY_JSON" '(.data.persisted == false)' "true" \
    "verification_replay_not_persisted"
assert_jq "$REPLAY_JSON" '.data.replayed // false' "true" \
    "verification_replay_flag"
assert_jq "$REPLAY_JSON" '.data.degradations[0] // empty' \
    "degraded.verification_idempotent_replay" \
    "verification_replay_degradation"

FALLBACK_EVIDENCE=$(jq -c 'map(select(.status == "fallback_detected"))[0]' "$GOLDEN_EVIDENCE")
FALLBACK_JSON=$(printf '%s' "$FALLBACK_EVIDENCE" \
    | ee_workspace verification ingest \
        --stdin \
        --target-type memory \
        --target-id "$MEMORY_ID" \
        --actor "verification_evidence_e2e" \
        --json 2>/dev/null || true)
assert_jq "$FALLBACK_JSON" '.data.verificationEvidence.status // empty' \
    "fallback_detected" \
    "verification_fallback_ingested"

GUIDANCE_JSON=$(ee_workspace verification closure-guidance \
    --bead-id bd-example \
    --require-rch-cargo \
    --json 2>/dev/null || true)
assert_jq "$GUIDANCE_JSON" '(.data.guidance.canClose == false)' "true" \
    "verification_closure_rejects_fallback"
REJECT_REASON=$(printf '%s' "$GUIDANCE_JSON" \
    | jq -r '.data.guidance.rejectedReasons[]? | select(test("local fallback"))' \
        2>/dev/null | head -n 1)
if [ -n "$REJECT_REASON" ]; then
    e2e_log_assert_eq "true" "true" "verification_closure_reason_mentions_fallback"
else
    e2e_log_assert_eq "missing local fallback reason" "present" \
        "verification_closure_reason_mentions_fallback"
fi
