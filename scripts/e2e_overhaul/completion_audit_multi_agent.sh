#!/usr/bin/env bash
# CA5 — no-mock completion-audit e2e with multi-agent evidence.
#
# Exercises the public handoff completion-audit command with evidence shaped
# like a coordinated swarm closeout: docs read, code inspection, Agent Mail,
# Beads, bv triage, and RCH-only verification proof.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "completion_audit_multi_agent"

OBJECTIVE_FILE="$EPIC_WORKSPACE/completion_audit_objective.txt"
EVIDENCE_FILE="$EPIC_WORKSPACE/completion_audit_evidence.json"
REPORT_FILE="$EPIC_WORKSPACE/completion_audit_report.json"

cat >"$OBJECTIVE_FILE" <<'EOF'
Read AGENTS.md and README.md. Perform code investigation for the completion
audit command. Coordinate through Agent Mail, track progress through Beads,
use the bv tool to prioritize, run cargo builds and tests through RCH, and
perform a prompt-to-artifact completion audit before claiming done.
EOF

cat >"$EVIDENCE_FILE" <<'EOF'
{
  "records": [
    {
      "kind": "file_read",
      "target": "AGENTS.md",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "AGENTS.md read before selecting bd-1zb7k.18.2"
    },
    {
      "kind": "prompt_requirement",
      "target": "AGENTS.md",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "AGENTS.md requirement explicitly satisfied"
    },
    {
      "kind": "file_read",
      "target": "README.md",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "README.md read for project context"
    },
    {
      "kind": "prompt_requirement",
      "target": "README.md",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "README.md requirement explicitly satisfied"
    },
    {
      "kind": "code_inspection",
      "target": "repository",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "Inspected src/core/completion_audit.rs and src/cli/mod.rs"
    },
    {
      "kind": "read_only_architecture_audit",
      "target": "repository",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "Verified completion-audit data flow from CLI to core report"
    },
    {
      "kind": "agent_mail",
      "target": "project inbox/outbox",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "Agent Mail thread bd-1zb7k.18.2 announced the active slice"
    },
    {
      "kind": "coordination_receipt",
      "target": "agent mail",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "Coordination event recorded for peer visibility"
    },
    {
      "kind": "beads",
      "target": ".beads/issues.jsonl",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "bd-1zb7k.18.2 claimed before implementation"
    },
    {
      "kind": "tracker_comment_or_status",
      "target": "beads",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "Bead status carries in-progress ownership"
    },
    {
      "kind": "bv",
      "target": "bv robot output",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "supporting",
      "summary": "bv --robot-next and bv --robot-triage used for selection"
    },
    {
      "kind": "triage_command",
      "target": "bv --robot-next or bv --robot-triage",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "Robot-mode bv output selected the work lane"
    },
    {
      "kind": "rch",
      "target": "remote build metadata",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "RCH-only policy proof recorded for cargo/build/test verification"
    },
    {
      "kind": "remote_rch",
      "target": "cargo/build/test command",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "Remote cargo proof or fail-closed static proof accompanies closeout"
    },
    {
      "kind": "completion_audit",
      "target": "prompt-to-artifact checklist",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "Completion audit command produced an objective checklist"
    },
    {
      "kind": "completion_audit",
      "target": "explicit objective requirements",
      "source": "agent:SwiftKnoll",
      "status": "pass",
      "strength": "direct",
      "summary": "Completion audit evaluated every explicit objective requirement"
    }
  ]
}
EOF

REPORT_JSON=$(ee_workspace handoff completion-audit \
    --objective-file "$OBJECTIVE_FILE" \
    --evidence-json "$EVIDENCE_FILE" \
    --json)
printf '%s\n' "$REPORT_JSON" >"$REPORT_FILE"

assert_jq "$REPORT_JSON" '(.schema | test("^ee[.]response[.]v[0-9]+$"))' "true" \
    "completion_audit_envelope_schema"
assert_jq "$REPORT_JSON" '.success' "true" "completion_audit_success"
assert_jq "$REPORT_JSON" '.data.schema' "ee.completion_audit.report.v2" \
    "completion_audit_report_schema"
assert_jq "$REPORT_JSON" '.data.completionVerdict' "complete" \
    "completion_audit_complete_verdict"
assert_jq "$REPORT_JSON" '.data.gaps | length' "0" "completion_audit_no_gaps"
assert_jq "$REPORT_JSON" '.data.localBuildPolicy.state' "remote_verified" \
    "completion_audit_remote_verified_policy"
assert_jq "$REPORT_JSON" \
    '[.data.evidenceByRequirement[] | select(.support == "direct")] | length' \
    "$(printf '%s' "$REPORT_JSON" | jq -r '.data.checklist.summary.requirementCount')" \
    "completion_audit_all_requirements_direct"

COMMAND_EVENT_COUNT=$(jq -s \
    '[.[] | select(.test_id == "completion_audit_multi_agent" and .schema == "ee.test_event.v1" and .kind == "command_end")] | length' \
    "$EE_TEST_LOG_PATH")
ASSERT_EVENT_COUNT=$(jq -s \
    '[.[] | select(.test_id == "completion_audit_multi_agent" and .schema == "ee.test_event.v1" and (.kind == "assert_ok" or .kind == "assert_fail"))] | length' \
    "$EE_TEST_LOG_PATH")
e2e_log_assert_num "$COMMAND_EVENT_COUNT" -ge 1 \
    "completion_audit_structured_command_events"
e2e_log_assert_num "$ASSERT_EVENT_COUNT" -ge 1 \
    "completion_audit_structured_assert_events"

_e2e_emit_event "completion_audit_multi_agent_summary" \
    "bead_id" "bd-1zb7k.18.2" \
    "requirements" "$(printf '%s' "$REPORT_JSON" | jq -r '.data.checklist.summary.requirementCount')" \
    "direct_requirements" "$(printf '%s' "$REPORT_JSON" | jq -r '[.data.evidenceByRequirement[] | select(.support == "direct")] | length')" \
    "local_build_policy" "$(printf '%s' "$REPORT_JSON" | jq -r '.data.localBuildPolicy.state')" \
    "report_path" "$REPORT_FILE"

if [ "$EE_TEST_LOG_ASSERTS_FAIL" -ne 0 ]; then
    echo "completion_audit_multi_agent: $EE_TEST_LOG_ASSERTS_FAIL assertions failed" >&2
    exit 1
fi

echo "completion_audit_multi_agent passed: report=$REPORT_FILE events=$EE_TEST_LOG_PATH" >&2
