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

EE_CI_PROOF_LANE_GH_BIN=/definitely/not/gh \
    "$SNAPSHOT_SCRIPT" \
    --head-sha 7044bf29b7d11fa76ca4a7af0c4a1abe0ad93939 \
    --json |
    jq -e '.summary.verdict == "gh_unavailable"
      and .summary.localCargoFallbackAllowed == false
      and .degraded[0].code == "ci_proof_lane_gh_unavailable"' >/dev/null

printf 'ci proof-lane snapshot fixture tests passed\n'
