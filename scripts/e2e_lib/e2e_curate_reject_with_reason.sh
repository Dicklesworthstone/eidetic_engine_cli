#!/usr/bin/env bash
# bd-3qs2i.1.5 — F1.5 e2e for `ee curate reject/accept --reason`.
#
# Exercises the full agent flow against a real ee binary in a fresh
# workspace. Each STEP corresponds to a user-flow assertion called out
# in the F1.5 bead.
#
# The bead outline suggests verifying the audit reason via
# `ee memory history <candidate-id>`, but `memory history` is keyed by
# memory_id and filters audit rows to `target_type = "memory"`. The
# curate.transition audit row has `target_type = "curation_candidate"`,
# so the canonical surface is `ee audit show <audit-id>`, which the
# curate response already returns at `data.mutation.auditId`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORKSPACE="${WORKSPACE:-$(mktemp -d -t ee-e2e-f1-XXXX)}"
export WORKSPACE

# shellcheck source=scripts/e2e_lib/agent_ergonomics_lib.sh
source "$SCRIPT_DIR/agent_ergonomics_lib.sh"

ee_workspace() {
    "$EE_BIN" --workspace "$WORKSPACE" "$@"
}

# Insert a fresh curation candidate via the diagnostic surface and
# return its id on stdout. Each invocation must produce a unique id.
# The id format is curate_<26 alphanumerics>.
seed_count=0
set_up_candidate() {
    seed_count=$((seed_count + 1))
    local suffix
    # 26-char suffix, padded with zeros.
    suffix=$(printf 'e2eF1seed%017d' "$seed_count")
    local cand_id="curate_${suffix}"
    "$EE_BIN" --workspace "$WORKSPACE" diag curation-candidate \
        --candidate-id "$cand_id" \
        --candidate-type rule \
        --status pending \
        --source-type human_request \
        --source-id "f1_e2e_$seed_count" \
        --reason "seeded for F1.5 e2e: slot $seed_count" \
        --allow-missing-target \
        --json >"$LOG_DIR/seed_${seed_count}.json"
    printf '%s' "$cand_id"
}

# ---------------------------------------------------------------------------
log_step "Initialize fresh workspace"
init_out=$(ee_workspace init --json)
assert_jq "$init_out" '.success' "true" "init returns success=true"

# ---------------------------------------------------------------------------
log_step "Reject with --reason; expect persisted audit row"
cand_reject=$(set_up_candidate)
reject_out=$(ee_workspace curate reject "$cand_reject" \
    --reason "duplicate evidence" --actor agent-e2e --json)
assert_jq "$reject_out" '.success' "true" "reject succeeds"
assert_jq "$reject_out" '.data.mutation.persisted' "true" "reject persists mutation"
reject_audit=$(printf '%s' "$reject_out" | jq -r '.data.mutation.auditId // ""')
if [ -z "$reject_audit" ]; then
    record_failure "reject_audit_id_present" "data.mutation.auditId missing"
else
    record_pass "reject_audit_id_present"
fi
audit_show=$(ee_workspace audit show "$reject_audit" --json)
assert_jq "$audit_show" '.data.row.details.reason' "duplicate evidence" \
    "reject audit row carries details.reason"
assert_jq "$audit_show" '.data.row.target_type' "curation_candidate" \
    "audit row targets curation_candidate"

# ---------------------------------------------------------------------------
log_step "Accept with --reason; symmetric audit shape"
cand_accept=$(set_up_candidate)
accept_out=$(ee_workspace curate accept "$cand_accept" \
    --reason "validated by humans" --actor agent-e2e --json)
assert_jq "$accept_out" '.success' "true" "accept succeeds"
accept_audit=$(printf '%s' "$accept_out" | jq -r '.data.mutation.auditId // ""')
accept_show=$(ee_workspace audit show "$accept_audit" --json)
assert_jq "$accept_show" '.data.row.details.reason' "validated by humans" \
    "accept audit row carries details.reason"

# ---------------------------------------------------------------------------
log_step "Dry-run reject returns plannedDetails.reason without writing audit"
cand_dry=$(set_up_candidate)
dry_out=$(ee_workspace curate reject "$cand_dry" \
    --reason "dry-run preview" --dry-run --actor agent-e2e --json)
assert_jq "$dry_out" '.success' "true" "dry-run succeeds"
assert_jq "$dry_out" '.data.dryRun' "true" "dryRun flag set"
assert_jq "$dry_out" '.data.plannedDetails.reason' "dry-run preview" \
    "plannedDetails.reason preview present"
assert_jq "$dry_out" '.data.mutation.persisted' "false" "dry-run does not persist"
assert_jq "$dry_out" '.data.mutation.auditId // \"absent\"' "absent" \
    "dry-run does not return an audit id"

# ---------------------------------------------------------------------------
log_step "Calls without --reason continue to work unchanged"
cand_no_reason=$(set_up_candidate)
no_reason_out=$(ee_workspace curate reject "$cand_no_reason" \
    --actor agent-e2e --json)
assert_jq "$no_reason_out" '.success' "true" "reject without --reason succeeds"
no_reason_audit=$(printf '%s' "$no_reason_out" | jq -r '.data.mutation.auditId // ""')
no_reason_show=$(ee_workspace audit show "$no_reason_audit" --json)
assert_jq "$no_reason_show" '.data.row.details.reason // \"null\"' "null" \
    "audit row omits reason when none supplied"

# ---------------------------------------------------------------------------
log_step "Oversized --reason rejected with curate_reason_too_large error"
cand_oversize=$(set_up_candidate)
long_reason=$(printf 'X%.0s' $(seq 1 5000))
set +e
oversize_out=$(ee_workspace curate reject "$cand_oversize" \
    --reason "$long_reason" --actor agent-e2e --json 2>/dev/null)
oversize_rc=$?
set -e
if [ "$oversize_rc" -eq 0 ]; then
    record_failure "oversize_reason_fails" \
        "expected non-zero exit, got 0; stdout=$oversize_out"
else
    record_pass "oversize_reason_fails"
fi
assert_jq "$oversize_out" '.success' "false" "error envelope: success=false"
assert_jq "$oversize_out" '.error.code' "curate_reason_too_large" \
    "error code is curate_reason_too_large"
assert_jq "$oversize_out" '(.error.details.recovery | type)' "array" \
    "error.details.recovery is an array"

# ---------------------------------------------------------------------------
log_step "tracing::info! span fires under EE_LOG_JSON=1 on real transitions"
cand_trace=$(set_up_candidate)
trace_log="$LOG_DIR/trace.log"
EE_LOG_JSON=1 "$EE_BIN" --workspace "$WORKSPACE" curate reject "$cand_trace" \
    --reason "trace coverage" --actor agent-e2e --json \
    >"$LOG_DIR/trace_stdout.json" 2>"$trace_log"
assert_contains "$(cat "$trace_log")" "ee::curate::transition" \
    "tracing target ee::curate::transition emitted"
assert_contains "$(cat "$trace_log")" "reason_present" \
    "trace event carries reason_present field"
# Confirm the reason text itself is NOT logged — only its presence / length.
if grep -q '"trace coverage"' "$trace_log"; then
    record_failure "reason_text_not_in_trace_log" \
        "reason string leaked into trace log"
else
    record_pass "reason_text_not_in_trace_log"
fi
