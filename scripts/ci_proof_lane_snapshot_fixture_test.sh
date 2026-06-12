#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SNAPSHOT_SCRIPT="${REPO_ROOT}/scripts/ci_proof_lane_snapshot.sh"
FIXTURE_DIR="${REPO_ROOT}/tests/fixtures/ci_proof_lane_live"

require_tool() {
    if ! command -v "$1" >/dev/null 2>&1; then
        printf 'ci_proof_lane_snapshot_fixture_test: required tool missing: %s\n' "$1" >&2
        exit 2
    fi
}

assert_fixture() {
    local fixture="$1"
    local jq_filter="$2"

    "$SNAPSHOT_SCRIPT" --input "${FIXTURE_DIR}/${fixture}" --json |
        jq -e "$jq_filter" >/dev/null
}

require_tool jq
require_tool ruby

bash -n "$SNAPSHOT_SCRIPT"

assert_fixture \
    duplicate_active_runs.json \
    '.schema == "ee.ci_proof_lane_snapshot.v1"
      and .summary.verdict == "duplicate_dispatch_detected"
      and .summary.activeRunCount == 2
      and .summary.localCargoFallbackAllowed == false
      and .activeRecommendation.nextAction == "wait"'

assert_fixture \
    active_run_stale.json \
    '.schema == "ee.ci_proof_lane_snapshot.v1"
      and .summary.verdict == "wait_for_active_run"
      and .summary.activeRunCount == 1
      and .activeRecommendation.nextAction == "wait"
      and ((.workflows[] | select(.workflowName == "macOS EE Artifact") | .runs[] | select(.runId == "27228688656") | .jobEvidence[0].labels) == ["macos-14"])
      and ((.workflows[] | select(.workflowName == "macOS EE Artifact") | .runs[] | select(.runId == "27228688656") | .jobEvidence[0].runnerAssignment) == "unassigned")
      and ((.workflows[] | select(.workflowName == "macOS EE Artifact") | .runs[] | select(.runId == "27228688656") | .jobEvidence[0].startedAt) == "2026-06-09T18:55:22Z")
      and ((.workflows[] | select(.workflowName == "macOS EE Artifact") | .runs[] | select(.runId == "27228688656") | .jobEvidence[0].queueAgeSeconds) == 2681)
      and ((.workflows[] | select(.workflowName == "macOS EE Artifact") | .runs[] | select(.runId == "27228688656") | .queueDiagnosis.status) == "github_hosted_runner_capacity")
      and ((.workflows[] | select(.workflowName == "macOS EE Artifact") | .runs[] | select(.runId == "27228688656") | .queueDiagnosis.comparablePriorRunId) == "27164008356")
      and ((.workflows[] | select(.workflowName == "macOS EE Artifact") | .runs[] | select(.runId == "27228688656") | .queueDiagnosis.nextAction) == "inspect_github_runner_capacity_or_labels")
      and (.degraded[] | select(.code == "ci_proof_lane_active_run_stale"))'

assert_fixture \
    missing_artifact.json \
    '.schema == "ee.ci_proof_lane_snapshot.v1"
      and .summary.verdict == "artifact_missing"
      and .summary.localCargoFallbackAllowed == false
      and .degraded[0].code == "ci_proof_lane_artifact_missing"
      and (.workflows[] | select(.workflowName == "macOS EE Artifact") | .runs[0].artifacts[0].status) == "missing"'

assert_fixture \
    stale_artifact.json \
    '.schema == "ee.ci_proof_lane_snapshot.v1"
      and .summary.verdict == "artifact_stale"
      and .summary.staleArtifactCount == 1
      and .summary.localCargoFallbackAllowed == false
      and .activeRecommendation.nextAction == "dispatch_new_run"'

assert_fixture \
    local_only_head_unavailable.json \
    '.schema == "ee.ci_proof_lane_snapshot.v1"
      and .repository.headShaReachability == "github_unreachable"
      and .summary.verdict == "local_only_head_unavailable"
      and .summary.localCargoFallbackAllowed == false
      and .summary.sourceTestVerdict == "not_evaluated"
      and .activeRecommendation.nextAction == "abstain_manual_review"
      and .degraded[0].code == "ci_proof_lane_local_only_head_unavailable"
      and .recoveryActions[0].kind == "manual_review"'

assert_fixture \
    checksum_mismatch.json \
    '.schema == "ee.ci_proof_lane_snapshot.v1"
      and .summary.verdict == "checksum_mismatch"
      and .summary.checksumMismatchCount == 1
      and .summary.sourceTestVerdict == "artifact_authority_only"
      and .summary.localCargoFallbackAllowed == false
      and .activeRecommendation.nextAction == "file_followup_bead"
      and .degraded[0].code == "ci_proof_lane_checksum_mismatch"
      and .degraded[0].severity == "high"
      and (.workflows[] | select(.workflowName == "macOS EE Artifact") | .runs[0].firstFailureDiagnosis | contains("checksum mismatch"))'

assert_fixture \
    surface_probe_failed.json \
    '.schema == "ee.ci_proof_lane_snapshot.v1"
      and .summary.verdict == "surface_probe_failed"
      and .summary.sourceTestVerdict == "artifact_authority_only"
      and .summary.localCargoFallbackAllowed == false
      and .activeRecommendation.nextAction == "file_followup_bead"
      and .degraded[0].code == "ci_proof_lane_surface_probe_failed"
      and .degraded[0].severity == "high"
      and (.workflows[] | select(.workflowName == "macOS EE Artifact") | .runs[0].artifacts[0].surfaceProbes[0].status) == "failed"'

EE_CI_PROOF_LANE_GH_BIN=/definitely/not/gh \
    "$SNAPSHOT_SCRIPT" \
    --head-sha 7044bf29b7d11fa76ca4a7af0c4a1abe0ad93939 \
    --json |
    jq -e '.summary.verdict == "gh_unavailable"
      and .summary.localCargoFallbackAllowed == false
      and .degraded[0].code == "ci_proof_lane_gh_unavailable"' >/dev/null

set +e
invalid_sha_stdout="$(
    "$SNAPSHOT_SCRIPT" \
        --input "${FIXTURE_DIR}/missing_artifact.json" \
        --head-sha 123 \
        --json 2>/dev/null
)"
invalid_sha_status=$?
set -e
if [ "$invalid_sha_status" -ne 2 ]; then
    printf 'ci_proof_lane_snapshot_fixture_test: invalid --head-sha should exit 2, got %s\n' "$invalid_sha_status" >&2
    exit 1
fi
if [ -n "$invalid_sha_stdout" ]; then
    printf 'ci_proof_lane_snapshot_fixture_test: invalid --head-sha wrote stdout\n' >&2
    exit 1
fi

printf 'ci proof-lane snapshot fixture tests passed\n'
