#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SNAPSHOT_DIR="${REPO_ROOT}/tests/snapshots/eval"
SELF_TEST_MISRANK=false

usage() {
    cat <<'USAGE'
Usage:
  scripts/eval_regression.sh
  scripts/eval_regression.sh --self-test-misrank-top1

Validates the bd-bife.18 pack-quality regression baseline reports:
  - NDCG@10 must not drop by more than 2pp
  - MRR must not drop by more than 3pp
  - Precision@5 must not drop by more than 5pp
  - Pack hash stability must hold across 3 runs

The self-test injects a deliberate top-1 mis-rank penalty and succeeds only
when the NDCG threshold catches that regression.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --self-test-misrank-top1)
            SELF_TEST_MISRANK=true
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
    shift
done

need_tool() {
    local tool="$1"
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: required tool not found: $tool" >&2
        exit 1
    fi
}

validate_metric() {
    local file="$1"
    local metric="$2"
    local allowed="$3"

    jq -e --arg metric "$metric" --argjson allowed "$allowed" '
        .metrics[$metric].allowedDropPp == $allowed
        and (.metrics[$metric].baseline | type == "number")
        and (.metrics[$metric].current | type == "number")
        and (.metrics[$metric].baseline >= .metrics[$metric].current)
        and (((.metrics[$metric].baseline - .metrics[$metric].current) * 100.0) <= ($allowed + 0.000001))
    ' "$file" >/dev/null
}

validate_report() {
    local file="$1"

    jq empty "$file"
    jq -e '
        .schema == "ee.eval.regression_report.v1"
        and .owningBeadId == "bd-bife.18"
        and (.feature | type == "string" and length > 0)
        and (.featureBeadId | type == "string" and startswith("bd-"))
        and .status == "passed"
        and .thresholds.ndcgAt10AllowedDropPp == 2.0
        and .thresholds.mrrAllowedDropPp == 3.0
        and .thresholds.precisionAt5AllowedDropPp == 5.0
        and .metrics.packHashStability.runs == 3
        and .metrics.packHashStability.stable == true
        and (.metrics.packHashStability.hashes | type == "array" and length == 3)
        and (.sourceEval.fixtureId | type == "string" and length > 0)
        and (.sourceEval.command | type == "string" and contains("ee eval"))
        and (.regressionProof.misrankTop1.penaltyPp | type == "number")
    ' "$file" >/dev/null

    validate_metric "$file" "ndcgAt10" 2.0
    validate_metric "$file" "mrr" 3.0
    validate_metric "$file" "precisionAt5" 5.0
}

misrank_top1_is_caught() {
    local file="$1"

    jq -e '
        . as $report
        | ($report.metrics.ndcgAt10.current - ($report.regressionProof.misrankTop1.penaltyPp / 100.0)) as $misranked
        | ((($report.metrics.ndcgAt10.baseline - $misranked) * 100.0) > ($report.metrics.ndcgAt10.allowedDropPp + 0.000001))
    ' "$file" >/dev/null
}

need_tool jq

expected_reports=(
    post_g1_ppr.json
    post_g2_pack_dna.json
    post_g3_causal.json
    post_g4_structural_health.json
    post_g5_curate_decay.json
    post_g6_gomory_hu.json
    post_g7_dominance.json
    post_g8_skyline.json
    post_g9_load_bearing.json
    post_g10_hits.json
)

if [ ! -d "$SNAPSHOT_DIR" ]; then
    echo "error: missing eval snapshot directory: $SNAPSHOT_DIR" >&2
    exit 1
fi

actual_count=$(find "$SNAPSHOT_DIR" -maxdepth 1 -type f -name 'post_*.json' | wc -l | tr -d ' ')
if [ "$actual_count" -ne "${#expected_reports[@]}" ]; then
    echo "error: expected ${#expected_reports[@]} eval reports, found $actual_count" >&2
    exit 1
fi

for report in "${expected_reports[@]}"; do
    path="${SNAPSHOT_DIR}/${report}"
    if [ ! -f "$path" ]; then
        echo "error: missing eval report: tests/snapshots/eval/${report}" >&2
        exit 1
    fi
    validate_report "$path"
done

if [ "$SELF_TEST_MISRANK" = "true" ]; then
    proof_path="${SNAPSHOT_DIR}/post_g1_ppr.json"
    if ! misrank_top1_is_caught "$proof_path"; then
        echo "error: injected top-1 mis-rank did not trip the NDCG@10 threshold" >&2
        exit 1
    fi
    echo "ok: deliberate top-1 mis-rank is caught by NDCG@10 threshold"
    exit 0
fi

echo "ok: ${#expected_reports[@]} eval regression reports passed bd-bife.18 thresholds"
