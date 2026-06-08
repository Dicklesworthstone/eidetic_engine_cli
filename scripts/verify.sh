#!/bin/bash
set -euo pipefail

# EE-TST-LP4P-GAP-001 / EE-TST-LP4P-GAP-004: Central Verification Runner
#
# This script orchestrates the readiness gates for Eidetic Engine (ee).
# It executes standard tests, forbidden dependency checks, and the
# complex E2E/boundary migration pipelines.
#
# Usage:
#   ./scripts/verify.sh                # Run the default profile (all correctness
#                                       # gates; benches/eval are opt-in)
#   ./scripts/verify.sh --ci-smoke      # Fast minimal gate set: forbidden deps,
#                                       # closure linter, drift guards, snapshot
#                                       # proposal guard, advisories, vision
#                                       # coverage, unit/contract/golden tests,
#                                       # Basic E2E. Skips heavy mesh/Tailscale,
#                                       # overhaul integration, advanced E2E,
#                                       # boundary migration, doctor safety
#                                       # harness, benches, and eval. Intended
#                                       # for swarm CI smoke + agent pre-push
#                                       # readiness without paying the full
#                                       # mesh/RCH cost. Documented in
#                                       # docs/operator-swarm-slo.md (bd-2dgn0.5).
#   ./scripts/verify.sh --swarm-heavy   # 64-agent / Swarm-X full verification:
#                                       # default profile PLUS plan-doc-smoke,
#                                       # fuzz-smoke, eval regression, and
#                                       # benches. Intended for large-host
#                                       # opt-in scorecard runs that feed the
#                                       # bd-2dgn0 swarm SLO evidence trail.
#                                       # Documented in docs/operator-swarm-slo.md.
#   ./scripts/verify.sh --plan-doc-smoke # Run plan-sweep verify_cmd smoke checks
#   ./scripts/verify.sh --fuzz-target-audit-self-test # Run only the no-Cargo fuzz audit matcher self-test
#   ./scripts/verify.sh --fuzz-smoke   # Include 30s cargo-fuzz query parser smoke
#   ./scripts/verify.sh --include-bench # Include performance benchmarks
#   ./scripts/verify.sh --eval          # Include pack-quality eval regression sweep
#   ./scripts/verify.sh --help         # Show this help
#
# Gates (in order):
#   0. Plan Doc Smoke        - optional bd-3usjw.23 verify_cmd manifest checks
#   0.9. Forbidden Dependency Contract - no-Cargo metadata scanner self-test
#   1. Forbidden Dependencies  - cargo tree audit for banned crates
#   2. Closure Linter          - prevent abstention-as-implementation closure
#   3. Snapshot Proposal Guard - block unreviewed tracked insta proposals
#   4. Untracked Work Audit    - advisory Beads FILE SURFACE coverage for dirty paths
#   4.49. Bridge Staleness Contract - no-Cargo bridge fixture scanner self-test
#   4.5. Bridge Staleness      - advisory signal when CLOSE_THE_GAP_PLAN needs refresh
#   4.59. Plan Drift Contract  - no-Cargo plan/bead fixture scanner self-test
#   4.6. Plan Drift Advisory   - advisory plan_doc_section drift hints for Beads triage
#   4.64. Tracing Field Contract - no-Cargo tracing manifest checker self-test
#   4.65. Contract Drift Radar - advisory schema/docs/taxonomy drift scanner (bd-31nul.5)
#   4.655. E2E Event Contract Radar Contract - no-Cargo golden report/schema harness
#   4.66. E2E Event Contract Radar - advisory shell evidence coverage scanner (bd-2ljka.4)
#   4.665. Work Packet No-Mutation - shell fixture matrix for claim-gate consumer safety
#   4.67. Panic Helper Radar Contract - no-Cargo schema/golden scanner contract gate
#   4.68. Swarm SLO Replay Contract - no-Cargo replay fixture/golden contract gate
#   4.69. CI Proof-Lane Snapshot Contract - no-Cargo proof-lane fixture gate
#   4.70. CI Proof-Lane Hygiene Contract - no-Cargo workflow policy self-test
#   4.705. CI Proof-Lane Hygiene Advisory - no-Cargo workflow policy scanner
#   4.71. RCH Doc Examples Contract - no-Cargo command classifier self-test
#   4.715. RCH Doc Examples Lint - no-Cargo docs command-shape scanner
#   4.72. Local Cargo Tripwire Contract - no-Cargo guardrail self-test
#   4.73. RCH Portability Diagnostic Contract - no-Cargo Mac-leak self-test
#   4.74. Package Artifact Leak Contract - no-Cargo deny-pattern self-test
#   4.75. Package Artifact Leak - cargo package list gate for generated artifacts
#   4.8. Fuzz Target Audit Contract - no-Cargo cargo-fuzz matcher self-test
#   4.81. Fuzz Target Audit     - static cargo-fuzz target registration/docs check
#   4.9. Fuzz Smoke            - optional 30s search query parser cargo-fuzz sweep
#   5. Vision Coverage         - report documented implemented/stubbed/missing surfaces
#   5.5. Proof Verification    - advisory Lean4/TLA+ proof artifact checks
#   6. Unit/Contract/Golden    - cargo test --workspace --lib --bins --tests --examples
#   6. Basic E2E               - scripts/e2e_test.sh
#   6.05 Output Budget E2E     - scripts/e2e_output_budget.sh
#   6.06 Replay Lab Smoke E2E  - scripts/e2e_overhaul/swarm_replay_lab_smoke.sh
#   6.07 Why-Not E2E          - scripts/e2e_why_not.sh
#   6.08 Cross-Cutting E2E     - scripts/e2e_cross_cutting.sh
#   6.09 Evidence Harvester E2E - scripts/e2e_evidence_harvester.sh
#   6.10 LOD Packing E2E       - scripts/e2e_lod_packing.sh
#   6.1. Agent Ergonomics E2E  - scripts/e2e_lib/run_agent_ergonomics_e2e.sh
#   6.5. Overhaul Integration  - scripts/e2e_overhaul.sh  (gated by VERIFY_OVERHAUL)
#   6.6. Fake Tailscale Harness - deterministic SRR6.46 fake tailnet self-test
#   7. Advanced E2E            - scripts/e2e_advanced.sh
#   8. Boundary Migration      - scripts/e2e_boundary_migration.sh
#   8.8. Eval Regression       - scripts/eval_regression.sh (optional)
#   9. Benchmarks (optional)   - scripts/bench_perf_regression.sh --check-regression
#
# Exit codes match AGENTS.md conventions (0=success, 1=usage, 3=storage, etc.)
# Artifacts are written to /tmp/ee-e2e-*/artifacts by E2E scripts.

INCLUDE_BENCH=false
INCLUDE_EVAL=false
INCLUDE_FUZZ_SMOKE=false
FUZZ_TARGET_AUDIT_SELF_TEST=false
PLAN_DOC_SMOKE=false
CI_SMOKE=false
SWARM_HEAVY=false
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"
BEADS_LOCK_WAIT_SECONDS="${EE_BEADS_LOCK_WAIT_SECONDS:-30}"
BEADS_LOCK_SKIP_CODE=75
VERIFY_BUDGET_FILE="${EE_VERIFY_BUDGET_FILE:-${SCRIPT_DIR}/verify-budget.toml}"
VERIFY_BUDGET_FAIL_CODE=6

for arg in "$@"; do
    case "$arg" in
        --help|-h)
            sed -n '3,62p' "$0" | sed 's/^# //' | sed 's/^#//'
            exit 0
            ;;
        --plan-doc-smoke)
            PLAN_DOC_SMOKE=true
            ;;
        --fuzz-target-audit-self-test)
            FUZZ_TARGET_AUDIT_SELF_TEST=true
            ;;
        --fuzz-smoke)
            INCLUDE_FUZZ_SMOKE=true
            ;;
        --include-bench)
            INCLUDE_BENCH=true
            ;;
        --eval)
            INCLUDE_EVAL=true
            ;;
        --ci-smoke)
            CI_SMOKE=true
            ;;
        --swarm-heavy)
            SWARM_HEAVY=true
            INCLUDE_BENCH=true
            INCLUDE_EVAL=true
            INCLUDE_FUZZ_SMOKE=true
            PLAN_DOC_SMOKE=true
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            exit 1
            ;;
    esac
done

if [ "$CI_SMOKE" = "true" ] && [ "$SWARM_HEAVY" = "true" ]; then
    echo "error: --ci-smoke and --swarm-heavy are mutually exclusive" >&2
    echo "       --ci-smoke trims to the fast minimal gate set;" >&2
    echo "       --swarm-heavy adds the heaviest opt-in gates." >&2
    echo "       See docs/operator-swarm-slo.md for guidance." >&2
    exit 1
fi

echo "=== EE Verification Runner ==="
if [ "$CI_SMOKE" = "true" ]; then
    echo "Profile: ci-smoke (fast minimal gate set; see docs/operator-swarm-slo.md)"
elif [ "$SWARM_HEAVY" = "true" ]; then
    echo "Profile: swarm-heavy (includes bench, eval, fuzz-smoke, plan-doc-smoke)"
else
    echo "Profile: default (correctness gates; benches and eval opt-in)"
fi
echo ""

if [ -d "${DEFAULT_AGENT_BUILD_ROOT}" ]; then
    mkdir -p "${DEFAULT_AGENT_BUILD_ROOT}/cargo-target" "${DEFAULT_AGENT_BUILD_ROOT}/tmp" 2>/dev/null || true
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${DEFAULT_AGENT_BUILD_ROOT}/cargo-target}"
    export TMPDIR="${EE_AGENT_TMPDIR:-${DEFAULT_AGENT_BUILD_ROOT}/tmp}"
fi

ARTIFACT_DIRS=""
TRACE_LOG_DIRS=""
STAGE_RESULTS=""
TOTAL_START=$(date +%s)

if [ -z "${EE_BINARY:-}" ]; then
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        export EE_BINARY="${CARGO_TARGET_DIR%/}/debug/ee"
    else
        export EE_BINARY="${REPO_ROOT}/target/debug/ee"
    fi
fi

# shellcheck disable=SC2329
beads_lock_wait_seconds() {
    case "$BEADS_LOCK_WAIT_SECONDS" in
        ''|*[!0-9]*)
            echo "error: EE_BEADS_LOCK_WAIT_SECONDS must be a non-negative integer" >&2
            exit 1
            ;;
        *)
            printf "%s" "$BEADS_LOCK_WAIT_SECONDS"
            ;;
    esac
}

# shellcheck disable=SC2329
with_beads_read_locks() {
    local beads_dir="${REPO_ROOT}/.beads"
    [ -d "$beads_dir" ] || {
        "$@"
        return $?
    }

    if ! command -v flock >/dev/null 2>&1; then
        echo "warning: flock not found; running Beads-reading gate without lock coordination" >&2
        "$@"
        return $?
    fi

    local wait_seconds
    wait_seconds=$(beads_lock_wait_seconds)

    local write_lock="${beads_dir}/.write.lock"
    local sync_lock="${beads_dir}/.sync.lock"

    if ! exec 8<>"$write_lock"; then
        echo "[!] SKIP: could not open Beads write lock $write_lock" >&2
        return "$BEADS_LOCK_SKIP_CODE"
    fi
    if ! flock -s -w "$wait_seconds" 8; then
        echo "[!] SKIP: Beads write lock is held: $write_lock" >&2
        return "$BEADS_LOCK_SKIP_CODE"
    fi

    if ! exec 9<>"$sync_lock"; then
        echo "[!] SKIP: could not open Beads sync lock $sync_lock" >&2
        flock -u 8 2>/dev/null || true
        exec 8>&- || true
        return "$BEADS_LOCK_SKIP_CODE"
    fi
    if ! flock -s -w "$wait_seconds" 9; then
        echo "[!] SKIP: Beads sync lock is held: $sync_lock" >&2
        flock -u 9 2>/dev/null || true
        exec 9>&- || true
        flock -u 8 2>/dev/null || true
        exec 8>&- || true
        return "$BEADS_LOCK_SKIP_CODE"
    fi

    set +e
    "$@"
    local status=$?
    set -e
    flock -u 9 2>/dev/null || true
    exec 9>&- || true
    flock -u 8 2>/dev/null || true
    exec 8>&- || true
    return "$status"
}

# shellcheck disable=SC2329
snapshot_proposal_guard() {
    if ! git -C "$REPO_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        echo "ok: not in a git worktree; snapshot proposal guard skipped"
        return 0
    fi

    local proposals
    proposals=$(git -C "$REPO_ROOT" ls-files | grep -E '\.snap\.new$' || true)
    if [ -z "$proposals" ]; then
        echo "ok: no tracked insta proposal snapshots"
        return 0
    fi

    local failures=0
    local count=0
    local proposal
    local accepted
    while IFS= read -r proposal; do
        [ -n "$proposal" ] || continue
        count=$((count + 1))
        accepted="${proposal%.new}"
        if ! git -C "$REPO_ROOT" ls-files --error-unmatch "$accepted" >/dev/null 2>&1; then
            echo "error: tracked insta proposal has no accepted snapshot: $proposal" >&2
            echo "       expected accepted snapshot: $accepted" >&2
            failures=1
            continue
        fi
        if ! cmp -s "$REPO_ROOT/$accepted" "$REPO_ROOT/$proposal"; then
            echo "error: tracked insta proposal differs from accepted snapshot: $proposal" >&2
            echo "       review with cargo insta and commit only accepted .snap files" >&2
            failures=1
        fi
    done <<< "$proposals"

    if [ "$failures" -ne 0 ]; then
        return 1
    fi
    echo "ok: $count tracked insta proposal snapshot(s) match accepted snapshots"
    echo "    removal of redundant .snap.new files still requires explicit approval"
}

# shellcheck disable=SC2329
fuzz_target_names() {
    printf '%s\n' \
        insights_section_dispatch \
        proximity_arg_parser \
        ppr_weight_clamp \
        insights_json_decode \
        search_query_parser
}

# shellcheck disable=SC2329
fuzz_manifest_has_registration() {
    local manifest="$1"
    local target="$2"
    local target_path="$3"

    awk -v name="name = \"${target}\"" -v path="path = \"${target_path}\"" '
        index($0, name) { has_name = 1 }
        index($0, path) { has_path = 1 }
        END { exit !(has_name && has_path) }
    ' "$manifest"
}

# shellcheck disable=SC2329
fuzz_target_file_has_shape() {
    local target_file="$1"

    awk '
        index($0, "#![no_main]") { has_no_main = 1 }
        index($0, "fuzz_target!") { has_fuzz_target = 1 }
        END { exit !(has_no_main && has_fuzz_target) }
    ' "$target_file"
}

# shellcheck disable=SC2329
fuzz_readme_has_sweep() {
    local readme="$1"
    local target="$2"
    local sweep_command="cargo fuzz run ${target} -- -max_total_time=300 -print_final_stats=1"

    grep -Fq "$sweep_command" "$readme"
}

# shellcheck disable=SC2329
fuzz_readme_has_global_proofs() {
    local readme="$1"

    awk '
        index($0, "Deliberate-panic proof") { has_deliberate_panic = 1 }
        index($0, "-max_total_time=900") { has_nightly_duration = 1 }
        END { exit !(has_deliberate_panic && has_nightly_duration) }
    ' "$readme"
}

# shellcheck disable=SC2329
fuzz_target_audit_self_test() {
    local manifest_good
    manifest_good='
[[bin]]
name = "insights_section_dispatch"
path = "fuzz_targets/insights_section_dispatch.rs"
[[bin]]
name = "proximity_arg_parser"
path = "fuzz_targets/proximity_arg_parser.rs"
[[bin]]
name = "ppr_weight_clamp"
path = "fuzz_targets/ppr_weight_clamp.rs"
[[bin]]
name = "insights_json_decode"
path = "fuzz_targets/insights_json_decode.rs"
[[bin]]
name = "search_query_parser"
path = "fuzz_targets/search_query_parser.rs"
'

    local readme_good
    readme_good='
cargo fuzz run insights_section_dispatch -- -max_total_time=300 -print_final_stats=1
cargo fuzz run proximity_arg_parser -- -max_total_time=300 -print_final_stats=1
cargo fuzz run ppr_weight_clamp -- -max_total_time=300 -print_final_stats=1
cargo fuzz run insights_json_decode -- -max_total_time=300 -print_final_stats=1
cargo fuzz run search_query_parser -- -max_total_time=300 -print_final_stats=1
Deliberate-panic proof
cargo fuzz run search_query_parser -- -max_total_time=900 -print_final_stats=1
'

    local source_good='#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    let _ = data;
});
'
    local source_bad='#![no_main]
pub fn placeholder() {}
'

    local target
    while IFS= read -r target; do
        [ -n "$target" ] || continue
        local target_path="fuzz_targets/${target}.rs"
        if ! fuzz_manifest_has_registration <(printf '%s\n' "$manifest_good") "$target" "$target_path"; then
            echo "error: fuzz target self-test expected manifest registration for ${target}" >&2
            return 1
        fi
        if ! fuzz_readme_has_sweep <(printf '%s\n' "$readme_good") "$target"; then
            echo "error: fuzz target self-test expected README sweep for ${target}" >&2
            return 1
        fi
        if ! fuzz_target_file_has_shape <(printf '%s\n' "$source_good"); then
            echo "error: fuzz target self-test expected valid target source shape" >&2
            return 1
        fi
    done < <(fuzz_target_names)

    if fuzz_manifest_has_registration <(printf '%s\n' "$manifest_good") search_query_parser "fuzz_targets/wrong.rs"; then
        echo "error: fuzz target self-test should reject mismatched manifest path" >&2
        return 1
    fi
    if fuzz_readme_has_sweep <(printf '%s\n' "cargo fuzz run search_query_parser") search_query_parser; then
        echo "error: fuzz target self-test should reject incomplete sweep command" >&2
        return 1
    fi
    if fuzz_target_file_has_shape <(printf '%s\n' "$source_bad"); then
        echo "error: fuzz target self-test should reject missing fuzz_target entrypoint" >&2
        return 1
    fi
    if ! fuzz_readme_has_global_proofs <(printf '%s\n' "$readme_good"); then
        echo "error: fuzz target self-test expected global README proof markers" >&2
        return 1
    fi

    echo "ok: fuzz target audit self-test passed"
}

# shellcheck disable=SC2329
fuzz_target_audit() {
    local manifest="${REPO_ROOT}/fuzz/Cargo.toml"
    local readme="${REPO_ROOT}/fuzz/README.md"
    local failures=0
    local target

    if [ ! -f "$manifest" ]; then
        echo "error: missing fuzz manifest: fuzz/Cargo.toml" >&2
        return 1
    fi
    if [ ! -f "$readme" ]; then
        echo "error: missing fuzz README: fuzz/README.md" >&2
        return 1
    fi

    while IFS= read -r target; do
        [ -n "$target" ] || continue
        local target_path="fuzz_targets/${target}.rs"
        local target_file="${REPO_ROOT}/fuzz/${target_path}"
        if ! fuzz_manifest_has_registration "$manifest" "$target" "$target_path"; then
            echo "error: fuzz/Cargo.toml missing bin/path registration for ${target}" >&2
            failures=1
        fi
        if [ ! -f "$target_file" ]; then
            echo "error: missing fuzz target file: fuzz/${target_path}" >&2
            failures=1
        elif ! fuzz_target_file_has_shape "$target_file"; then
            echo "error: fuzz/${target_path} is missing #![no_main] or fuzz_target! entrypoint" >&2
            failures=1
        fi
        if ! fuzz_readme_has_sweep "$readme" "$target"; then
            echo "error: fuzz/README.md missing 5-minute logged cargo-fuzz sweep command for ${target}" >&2
            failures=1
        fi
    done < <(fuzz_target_names)

    if ! fuzz_readme_has_global_proofs "$readme"; then
        echo "error: fuzz/README.md missing deliberate-panic proof instructions or 15-minute nightly cargo-fuzz sweep duration" >&2
        failures=1
    fi

    if [ "$failures" -ne 0 ]; then
        return 1
    fi

    echo "ok: bd-bife.10 fuzz targets are registered, present, documented with 5-minute logged sweeps plus nightly duration, and shaped as cargo-fuzz harnesses"
}

# shellcheck disable=SC2329
fuzz_smoke() {
    if ! command -v cargo >/dev/null 2>&1; then
        echo "error: cargo is required for fuzz smoke" >&2
        return 1
    fi
    if ! cargo fuzz --help >/dev/null 2>&1; then
        echo "error: cargo-fuzz is required for fuzz smoke" >&2
        return 1
    fi

    (
        cd "$REPO_ROOT"
        cargo +nightly fuzz run search_query_parser -- -max_total_time=30 -print_final_stats=1
    )
}

test_trace_root() {
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        printf "%s/ee-test-tracing" "${CARGO_TARGET_DIR%/}"
    else
        printf "%s/target/ee-test-tracing" "$REPO_ROOT"
    fi
}

capture_test_trace_artifacts() {
    local name="$1"
    local trace_root
    trace_root="$(test_trace_root)"

    if [ -d "$trace_root" ] &&
        find "$trace_root" -type f -name '*.jsonl' -print -quit 2>/dev/null | grep -q .; then
        TRACE_LOG_DIRS="${TRACE_LOG_DIRS}  ${name}: ${trace_root}\n"
    fi
}

stage_budget_value() {
    local stage_name="$1"
    local field="$2"

    [ -f "$VERIFY_BUDGET_FILE" ] || return 1

    awk -v target="$stage_name" -v field="$field" '
        /^\[\[stage\]\]/ {
            in_stage = 1
            matched = 0
            next
        }
        in_stage && /^name[[:space:]]*=/ {
            value = $0
            sub(/^[^=]*=[[:space:]]*/, "", value)
            gsub(/^"|"$/, "", value)
            matched = (value == target)
            next
        }
        in_stage && matched && $0 ~ ("^" field "[[:space:]]*=") {
            value = $0
            sub(/^[^=]*=[[:space:]]*/, "", value)
            gsub(/#.*/, "", value)
            gsub(/[[:space:]]+$/, "", value)
            gsub(/^"|"$/, "", value)
            print value
            found = 1
            exit
        }
        END {
            if (!found) {
                exit 1
            }
        }
    ' "$VERIFY_BUDGET_FILE"
}

stage_budget_thresholds() {
    local stage_name="$1"
    local p50
    local regression_factor

    p50="$(stage_budget_value "$stage_name" expected_seconds_p50)" || return 1
    regression_factor="$(stage_budget_value "$stage_name" regression_factor)" || return 1

    awk -v p50="$p50" -v regression_factor="$regression_factor" '
        BEGIN {
            advisory = int((p50 * regression_factor) + 0.999999)
            fail = int((p50 * 3) + 0.999999)
            printf "%d %d %d", p50, advisory, fail
        }
    '
}

stage_budget_summary() {
    local stage_name="$1"
    local duration="$2"
    local thresholds

    thresholds="$(stage_budget_thresholds "$stage_name")" || {
        printf "budget=untracked"
        return 0
    }

    local p50
    local advisory
    local fail
    read -r p50 advisory fail <<< "$thresholds"

    if [ "$duration" -gt "$fail" ]; then
        printf "budget=fail elapsed=%ss p50=%ss advisory=%ss fail=%ss" "$duration" "$p50" "$advisory" "$fail"
    elif [ "$duration" -gt "$advisory" ]; then
        printf "budget=advisory elapsed=%ss p50=%ss advisory=%ss fail=%ss" "$duration" "$p50" "$advisory" "$fail"
    else
        printf "budget=ok elapsed=%ss p50=%ss advisory=%ss fail=%ss" "$duration" "$p50" "$advisory" "$fail"
    fi
}

enforce_stage_budget() {
    local stage_name="$1"
    local duration="$2"
    local thresholds

    thresholds="$(stage_budget_thresholds "$stage_name")" || return 0

    local p50
    local advisory
    local fail
    read -r p50 advisory fail <<< "$thresholds"

    if [ "$duration" -gt "$fail" ]; then
        echo "error: verification stage exceeded hard budget: $stage_name" >&2
        echo "       elapsed=${duration}s p50=${p50}s hard_fail=${fail}s" >&2
        echo "       update scripts/verify-budget.toml only after validating the regression is expected" >&2
        return "$VERIFY_BUDGET_FAIL_CODE"
    fi

    if [ "$duration" -gt "$advisory" ]; then
        echo "[!] BUDGET: $stage_name exceeded advisory budget (${duration}s > ${advisory}s; p50=${p50}s)" >&2
    fi
}

run_stage() {
    local name="$1"
    local cmd="$2"
    echo "[*] Running: $name"
    echo "    $cmd"

    local start_time
    start_time=$(date +%s)
    local output_file
    output_file=$(mktemp)

    if eval "$cmd" 2>&1 | tee "$output_file"; then
        local end_time
        end_time=$(date +%s)
        local duration=$((end_time - start_time))
        local budget_summary
        budget_summary="$(stage_budget_summary "$name" "$duration")"
        echo "[+] PASS: $name (${duration}s; ${budget_summary})"
        STAGE_RESULTS="${STAGE_RESULTS}PASS ${name} (${duration}s; ${budget_summary})\n"
        capture_test_trace_artifacts "$name"

        # Capture artifact paths from E2E output
        local artifacts
        artifacts=$(grep -o 'Artifacts:[[:space:]]*[^ ]*' "$output_file" | head -1 | sed 's/Artifacts:[[:space:]]*//' || true)
        if [ -n "$artifacts" ] && [ -d "$artifacts" ]; then
            ARTIFACT_DIRS="${ARTIFACT_DIRS}  ${name}: ${artifacts}\n"
        fi
        rm -f "$output_file"
        enforce_stage_budget "$name" "$duration"
        echo ""
    else
        local exit_code=$?
        local end_time
        end_time=$(date +%s)
        local duration=$((end_time - start_time))
        if [ "$exit_code" -eq "$BEADS_LOCK_SKIP_CODE" ]; then
            local budget_summary
            budget_summary="$(stage_budget_summary "$name" "$duration")"
            echo "[!] SKIP: $name (${duration}s; ${budget_summary})"
            STAGE_RESULTS="${STAGE_RESULTS}SKIP ${name} (${duration}s; ${budget_summary})\n"
            rm -f "$output_file"
            enforce_stage_budget "$name" "$duration"
            echo ""
            return 0
        fi
        echo "[-] FAIL: $name (Exit code: $exit_code, ${duration}s)"
        rm -f "$output_file"
        exit $exit_code
    fi
}

# shellcheck disable=SC2329
plan_doc_smoke() {
    if ! command -v python3 >/dev/null 2>&1; then
        echo "error: python3 is required for --plan-doc-smoke" >&2
        return 1
    fi

    python3 - "$REPO_ROOT" <<'PY'
import json
import os
import subprocess
import sys
import time
from pathlib import Path

root = Path(sys.argv[1])
report = root / "docs" / "plan-sweep-report.md"
request_id = os.environ.get("EE_REQUEST_ID", "plan-doc-smoke")


def trace(phase, section_id="", elapsed_ms=0, degraded_codes=None):
    print(
        json.dumps(
            {
                "workspace_id": str(root),
                "request_id": request_id,
                "bead_id": "bd-3usjw.23",
                "surface": "plan_doc_verify_cmds",
                "phase": phase,
                "section_id": section_id,
                "elapsed_ms": elapsed_ms,
                "degraded_codes": degraded_codes or [],
            },
            separators=(",", ":"),
        ),
        file=sys.stderr,
    )


if not report.exists():
    trace("input", degraded_codes=["plan_sweep_report_missing"])
    print(f"error: missing plan sweep report: {report}", file=sys.stderr)
    sys.exit(1)

trace("input")
commands = []
in_matrix = False
for line in report.read_text(encoding="utf-8").splitlines():
    stripped = line.strip()
    if stripped == "## Machine-Checked Section Matrix":
        in_matrix = True
        continue
    if in_matrix and stripped.startswith("## "):
        break
    if not in_matrix or not stripped.startswith("|"):
        continue
    if "section_id" in stripped or "------------" in stripped:
        continue

    cells = [cell.strip() for cell in stripped.strip("|").split("|")]
    if len(cells) != 6:
        trace("dependency_check", degraded_codes=["plan_sweep_row_malformed"])
        print(f"error: plan matrix row must have 6 cells: {line}", file=sys.stderr)
        sys.exit(1)

    section_id, _title, _classification, _evidence, _test_bead, verify_cmd = cells
    if verify_cmd and verify_cmd != "-":
        commands.append((section_id, verify_cmd))

if not commands:
    trace("dependency_check", degraded_codes=["plan_sweep_verify_cmds_missing"])
    print("error: no plan sweep verify_cmd entries found", file=sys.stderr)
    sys.exit(1)

failures = 0
for section_id, command in commands:
    start = time.monotonic()
    print(f"[*] {section_id}: {command}")
    trace("dependency_check", section_id=section_id)
    try:
        result = subprocess.run(
            ["bash", "-lc", f"set -euo pipefail; {command}"],
            cwd=root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        elapsed_ms = int((time.monotonic() - start) * 1000)
        trace(
            "response",
            section_id=section_id,
            elapsed_ms=elapsed_ms,
            degraded_codes=["verify_cmd_timeout"],
        )
        print(f"error: {section_id} verify_cmd exceeded 60s: {command}", file=sys.stderr)
        if error.stdout:
            print(error.stdout, end="")
        if error.stderr:
            print(error.stderr, end="", file=sys.stderr)
        failures += 1
        continue

    elapsed_ms = int((time.monotonic() - start) * 1000)
    if result.stdout:
        print(result.stdout, end="")
    if result.stderr:
        print(result.stderr, end="", file=sys.stderr)

    if result.returncode == 0:
        trace("response", section_id=section_id, elapsed_ms=elapsed_ms)
    else:
        trace(
            "response",
            section_id=section_id,
            elapsed_ms=elapsed_ms,
            degraded_codes=["verify_cmd_failed"],
        )
        print(
            f"error: {section_id} verify_cmd exited {result.returncode}: {command}",
            file=sys.stderr,
        )
        failures += 1

if failures:
    sys.exit(1)

trace("response")
print(f"[+] plan-doc-smoke passed {len(commands)} verify commands")
PY
}

# shellcheck disable=SC2329
closure_lint_or_tracked_drift() {
    # The closure-lint audit covers both bead closure discipline and the
    # failure-mode fixture taxonomy (including *_unimplemented honesty-only
    # markers), so verify routes that full gate through drift tracking.
    if with_beads_read_locks ./scripts/closure-lint.sh --audit --json; then
        return 0
    fi

    local closure_exit=$?
    if with_beads_read_locks ./scripts/verification-drift-guard.sh --gate=closure-lint --json; then
        echo "[!] Closure linter reported tracked violations; continuing via Verification Drift Guard"
        return 0
    fi

    return "$closure_exit"
}

# shellcheck disable=SC2329
e2e_event_contract_radar_advisory() {
    local report="${EE_E2E_EVENT_CONTRACT_RADAR_REPORT:-${REPO_ROOT}/.e2e-event-contract-radar-report.json}"
    local allowlist="${EE_E2E_EVENT_CONTRACT_RADAR_ALLOWLIST:-}"
    local args=(--json --quiet --output "$report")

    if [ -n "$allowlist" ]; then
        args+=(--allowlist "$allowlist")
    fi

    local json
    json=$("${REPO_ROOT}/scripts/e2e_event_contract_radar.sh" "${args[@]}")

    printf "%s\n" "$json" | jq -r --arg report "$report" '
        "e2e event contract radar: verdict=\(.verdict) scripts=\(.summary.scriptCount) pass=\(.summary.passCount) advisory_gap=\(.summary.advisoryGapCount) known_gap=\(.summary.knownGapCount) fail=\(.summary.failCount) missing_failure_verdicts=\(.summary.missingFailureVerdictCount)",
        "report: \($report)"
    '
}

artifact_retention_summary() {
    echo ""
    echo "Artifact retention:"

    if [ ! -x "${EE_BINARY:-}" ]; then
        echo "  skipped: ee binary not found at ${EE_BINARY:-<unset>}"
        return 0
    fi

    local summary_json
    if ! summary_json=$("$EE_BINARY" --workspace "$REPO_ROOT" diag artifacts --json 2>/dev/null); then
        echo "  skipped: ee diag artifacts failed"
        return 0
    fi

    if command -v jq >/dev/null 2>&1; then
        printf "%s\n" "$summary_json" | jq -r '
            .data.summary
            | "  roots=\(.rootCount) existing=\(.existingRoots) bytes=\(.totalBytes) over_budget=\(.overBudgetRoots) expired=\(.expiredRoots)"
        ' || true
    else
        echo "  report available via: $EE_BINARY --workspace $REPO_ROOT diag artifacts --json"
    fi
}

if [ "$FUZZ_TARGET_AUDIT_SELF_TEST" = "true" ]; then
    fuzz_target_audit_self_test
    exit 0
fi

if [ "$PLAN_DOC_SMOKE" = "true" ]; then
    run_stage "Plan Doc Smoke (bd-3usjw.23)" "plan_doc_smoke"
    echo "=== Verification Summary ==="
    printf "%b" "$STAGE_RESULTS"
    exit 0
fi

# Gate 0.9: Forbidden dependency scanner contract. This no-Cargo self-test
# proves the JSON metadata classifier catches forbidden crates before the live
# cargo-tree audit runs.
run_stage "Forbidden Dependency Contract" "./scripts/check-forbidden-deps.sh --self-test"

# Gate 1: Check Forbidden Dependencies
run_stage "Forbidden Dependencies" "./scripts/check-forbidden-deps.sh"

# Gate 2: Closure Discipline
run_stage "Closure Linter" "closure_lint_or_tracked_drift"

# Gate 2.5: Drift Guard (ensures red gates have tracking beads)
run_stage "Verification Drift Guard" "with_beads_read_locks ./scripts/verification-drift-guard.sh --json"

# Gate 3: Snapshot Proposal Guard
run_stage "Snapshot Proposal Guard" "snapshot_proposal_guard"

# Gate 3.5: Advisory dirty-work ownership coverage. This remains advisory while
# multi-agent sessions routinely carry unrelated in-flight changes.
run_stage "Untracked Work Audit (advisory)" "with_beads_read_locks ./scripts/untracked-work-audit.sh"

# Gate 3.59: Bridge staleness contract. This no-Cargo fixture harness proves
# signal classifications before the live advisory scan reads Beads state.
run_stage "Bridge Staleness Contract" "./scripts/bridge-staleness.sh --self-test"

# Gate 3.6: Advisory bridge-plan staleness. This always exits 0 and writes
# .bridge-staleness-report.json so the trailing verify summary includes whether
# Part II appears stale enough to plan the next bridge.
run_stage "Bridge Staleness Advisory" "with_beads_read_locks ./scripts/bridge-staleness.sh --quiet"

# Gate 3.69: Plan/bead drift contract. This no-Cargo fixture harness proves
# warning classifications and BV hints before the live advisory scan reads
# Beads state.
run_stage "Plan Drift Contract" "./scripts/plan-drift.sh --self-test"

# Gate 3.7: Advisory plan/bead drift. This always exits 0 and writes
# .plan-drift-report.json with BV-friendly warning hints for active
# implements-surface beads whose plan_doc_section labels point at evolved text.
run_stage "Plan Drift Advisory" "with_beads_read_locks ./scripts/plan-drift.sh --quiet"

# Gate 3.79: Tracing field contract. This no-Cargo self-test validates the
# synthetic checker path for Part II tracing declarations and dueling-wizards
# observability manifests before later real-binary cross-cutting E2E coverage.
run_stage "Tracing Field Contract" "./scripts/check-tracing-fields.sh --self-test"

# Gate 3.8: Advisory contract-drift radar (bd-31nul.5). Cargo-free static
# scan of current-facing agent docs for stale envelope versions, unknown
# JSONC envelope schema ids, and degraded-code documentation that lacks a
# matching tests/fixtures/failure_modes/<code>.json fixture. Always exits 0
# and writes .contract-drift-radar-report.json with schema
# "ee.contract_drift_radar.v1". The Cargo-backed proof (full JSONC envelope
# validation against jsonschema files) is the schema_drift contracts test
# under cargo test -p ee --test contracts and stays an RCH-only surface.
run_stage "Contract Drift Radar Advisory" "./scripts/contract-drift-radar.sh --quiet"

# Gate 3.81: Deterministic self-test for the static radar's own report/event
# contract. This is shell-only and does not run Cargo or RCH.
run_stage "Contract Drift Radar Self-Test" "./scripts/contract-drift-radar.sh --self-test"

# Gate 3.84: E2E event-contract radar golden contract. This no-Cargo harness
# freezes the scanner report matrix, schema strictness, and negative fixture
# before the live advisory scan reads the full shell E2E surface.
run_stage "E2E Event Contract Radar Contract" "./scripts/e2e_event_contract_radar_golden.sh"

# Gate 3.85: Advisory e2e event-contract radar (bd-2ljka.4). This is a
# no-Cargo static scanner for shell E2E evidence logging. It writes
# .e2e-event-contract-radar-report.json by default and does not fail the
# readiness gate for advisory or known gaps; scanner/runtime errors still fail.
run_stage "E2E Event Contract Radar Advisory" "e2e_event_contract_radar_advisory"

# Gate 3.855: Work-packet no-mutation contract. This shell-only harness proves
# packet generation and the agent-facing claim-gate consumer stay read-only,
# refuse unsafe claim states, and include install-check freshness fixtures.
run_stage "Work Packet No-Mutation Contract" "./scripts/e2e_swarm_work_packet_no_mutation.sh"

# Gate 3.86: Panic-helper radar contract (bd-ppbue.30). This no-Cargo harness
# validates ee.panic_helper_radar.v1 schema/golden fixtures so scanner drift is
# caught without scanning the entire legacy Rust tree.
run_stage "Panic Helper Radar Contract" "./scripts/panic_helper_radar_golden.sh"

# Gate 3.87: Swarm SLO replay contract (bd-ppbue.31). This no-Cargo harness
# replays the compact swarm trace fixture, checks deterministic tie ordering,
# verifies summary schema/mutation flags, and fails before Cargo-backed gates
# if the shell replay contract drifts.
run_stage "Swarm SLO Replay Contract" "./scripts/e2e_overhaul/swarm_slo_replay.sh"

# Gate 3.88: CI proof-lane snapshot contract (bd-1n3x1.7). This no-Cargo
# harness transforms offline proof-lane fixtures and verifies duplicate-run,
# missing/stale artifact, checksum, surface-probe, unavailable-gh, and invalid
# SHA behavior before agents rely on CI artifact source-authority evidence.
run_stage "CI Proof-Lane Snapshot Contract" "./scripts/ci_proof_lane_snapshot_fixture_test.sh"

# Gate 3.89: CI proof-lane hygiene contract. This no-Cargo synthetic harness
# exercises workflow-dispatch, duplicate-dispatch, cancellable CI artifacts,
# release artifacts, and unclassified artifact-lane policy without reading
# live workflows or invoking Cargo.
run_stage "CI Proof-Lane Hygiene Contract" "./scripts/ci_proof_lane_hygiene.sh --self-test"

# Gate 3.895: CI proof-lane hygiene advisory (bd-1n3x1.8). This no-Cargo,
# network-free workflow scanner emits ee.ci_proof_lane_hygiene.v1 so agents see
# duplicate-dispatch, cancel-in-progress, artifact-retention, release-artifact,
# and unclassified artifact-lane posture before spending CI/RCH proof slots.
run_stage "CI Proof-Lane Hygiene Advisory" "./scripts/ci_proof_lane_hygiene.sh --json"

# Gate 3.90: RCH doc examples classifier contract. This no-Cargo self-test
# proves the command classifier denies local Cargo examples while accepting
# RCH-wrapped proof recipes before the live docs scan.
run_stage "RCH Doc Examples Contract" "python3 scripts/check-rch-doc-examples.py --self-test"

# Gate 3.905: RCH doc examples lint (bd-1n3x1.9). This no-Cargo docs scanner
# fails before expensive gates if AGENTS.md, README.md, or the RCH runbooks grow
# copy-pasteable local Cargo compile examples that bypass the verifier wrapper.
run_stage "RCH Doc Examples Lint" "python3 scripts/check-rch-doc-examples.py --json"

# Gate 3.91: Local Cargo tripwire contract (bd-1n3x1.10). This deterministic
# self-test validates command-shape denials, JSON repair actions, and fixture
# process classification without scanning live peer processes or running Cargo.
run_stage "Local Cargo Tripwire Contract" "./scripts/check-local-cargo-tripwire.sh --self-test"

# Gate 3.92: RCH portability diagnostic contract (bd-1n3x1.10). This
# deterministic self-test verifies the Mac-leak anomaly detector for remote
# transcripts without mutating workers, launching RCH, or deleting artifacts.
run_stage "RCH Portability Diagnostic Contract" "./scripts/check-rch-portability.sh --self-test"

# Gate 4.74: Package artifact leakage self-test. This proves the manifest
# exclude set and forbidden path classifier before the live cargo package list
# gate, without invoking Cargo.
run_stage "Package Artifact Leak Contract" "./scripts/package-artifact-leak-check.sh --self-test"

# Gate 4.75: Package artifact leakage guard. This is a quick packaging gate:
# it runs cargo package --list without building and fails if local/generated
# tracker, perf, backup, or temp artifact paths would enter the published crate.
run_stage "Package Artifact Leak Check (bd-2ifvx)" "./scripts/package-artifact-leak-check.sh"

# Gate 4.8: Fuzz target audit contract. This no-Cargo self-test proves the
# manifest, target source, README sweep, and nightly-proof matchers before the
# live static cargo-fuzz target registration/docs audit.
run_stage "Fuzz Target Audit Contract" "fuzz_target_audit_self_test"

# Gate 4.81: Static cargo-fuzz target registration/docs audit. This is a
# no-build guard; actual cargo-fuzz sweeps remain explicit RCH-only evidence.
run_stage "Fuzz Target Audit (bd-bife.10)" "fuzz_target_audit"

if [ "$INCLUDE_FUZZ_SMOKE" = "true" ]; then
    run_stage "Fuzz Smoke: search query parser (bd-2j2h0)" "fuzz_smoke"
fi

# Gate 4: Strategic Vision Coverage
run_stage "Vision Coverage" "with_beads_read_locks sh ./scripts/vision-coverage.sh --json"

# Gate 4.5: Mechanized proof artifacts. Missing Lean4/TLA+ tools degrade
# inside the driver instead of blocking the default readiness gate.
# Skipped under --ci-smoke because the Lean4/TLA+ driver depends on
# optional external toolchains that smoke runs should not require.
if [ "$CI_SMOKE" != "true" ]; then
    run_stage "Proof Verification (bd-nnfq4)" "./scripts/e2e_overhaul/proof_verify.sh"
else
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Proof Verification (bd-nnfq4) (ci-smoke)\n"
fi

# Gate 5: Core Cargo Tests (Contracts, Logic, Golden). Benchmarks are
# deliberately excluded here and run only through the explicit benchmark gate.
run_stage "Unit, Contract, and Golden Tests" "cargo test --workspace --lib --bins --tests --examples"

# Gate 6: Basic End-to-End
run_stage "Basic E2E Scripts" "./scripts/e2e_test.sh"

# Gate 6.05: Agent-facing output budget guard for status and swarm brief.
run_stage "Output Budget E2E (bd-kua65)" "./scripts/e2e_output_budget.sh"

# Gate 6.06: Replay lab smoke. This is intentionally no-Cargo and exercises
# the public `ee lab swarm replay --dry-run` path plus ee.test_event.v1 logging
# before the heavier replay/RCH proof lanes.
run_stage "Replay Lab Smoke E2E (bd-ppbue.8)" "./scripts/e2e_overhaul/swarm_replay_lab_smoke.sh"

# Gate 6.07: Dueling Wizards why-not real-binary E2E.
run_stage "Dueling Wizards Why-Not E2E" "./scripts/e2e_why_not.sh"

# Gate 6.08: Dueling Wizards cross-cutting static E2E. This is intentionally
# no-Cargo and checks the shared manifests/static gates before the feature
# scripts that depend on them.
run_stage "Dueling Wizards Cross-Cutting Static E2E" "./scripts/e2e_cross_cutting.sh"

# Gate 6.09: Dueling Wizards evidence-harvester real-binary E2E. Real-binary,
# no-Cargo: the script self-guards (log_drop) when the harvest/calibration CLI
# is not yet built into the binary, so it never false-fails the gate.
run_stage "Dueling Wizards Evidence Harvester E2E" "./scripts/e2e_evidence_harvester.sh"

# Gate 6.10: Dueling Wizards LOD packing real-binary E2E. Real-binary, no-Cargo:
# hard-asserts pack budget + hash determinism; condition-guards (log_drop) the
# peripheral-index/link-only tier and the cli-gated --no-lod parity.
run_stage "Dueling Wizards LOD Packing E2E" "./scripts/e2e_lod_packing.sh"

# Heavy gate block: skipped under --ci-smoke for fast swarm-CI / agent
# pre-push runs. bd-2dgn0.5: see docs/operator-swarm-slo.md for which
# gates are dropped and how to recover coverage in a follow-up
# --swarm-heavy run.
if [ "$CI_SMOKE" != "true" ]; then
    # Gate 6.1: Agent ergonomics F1-F5 e2e library driver. Missing future scripts
    # are reported as skips until their implementation beads land.
    run_stage "Agent Ergonomics E2E (F1-F5)" "./scripts/e2e_lib/run_agent_ergonomics_e2e.sh"

    # Gate 6.5: Overhaul Integration (J4). Gated behind VERIFY_OVERHAUL=1
    # until enough implementation beads ship to make the suite reliably
    # pass across CI. The driver itself respects VERIFY_OVERHAUL=0 and
    # exits 0 without running, so this stage stays fast in default CI.
    run_stage "Overhaul Integration E2E (J4)" "./scripts/e2e_overhaul.sh"

    # Gate 6.5.2: Lightweight swarm next-action recommendation-card contract.
    # This keeps SWA6's golden next-action overlap proof in the default gate
    # without requiring the heavier no-mock multi-agent harness.
    run_stage "Swarm Next-Action Recommendation Cards E2E (bd-3vwx0.6)" "./scripts/e2e_overhaul/swarm_next_action_recommendation_cards.sh"

    # Gate 6.6: Graph determinism harness (F4.a). This is separate from the J4
    # epic registry because it tracks the GraphAccretion surfaces while they are
    # landing incrementally.
    run_stage "Graph Determinism E2E (F4.a)" "./scripts/e2e_overhaul/graph_determinism.sh"

    # Gate 6.7: Fake Tailscale harness (SRR6.46.10). Later SRR6.46 auto-enrollment
    # e2e scripts import this library, so this self-test runs before those surfaces.
    run_stage "Fake Tailscale Harness E2E (SRR6.46.10)" "./scripts/e2e_overhaul/lib/test_fake_tailscale.sh"

    # Gate 6.8: Local Tailscale probe status harness (SRR6.46.1). This keeps the
    # no-network status surface covered by the deterministic fake Tailscale CLI.
    run_stage "Tailscale Local Probe E2E (SRR6.46.1)" "./scripts/e2e_overhaul/tailscale_local_probe.sh"

    # Gate 6.9: Tailscale peer autodiscovery harness (SRR6.46.2). Uses fake
    # Tailscale peer metadata; the script itself never invokes cargo.
    run_stage "Tailscale Peer Autodiscovery E2E (SRR6.46.2)" "./scripts/e2e_overhaul/tailscale_peer_autodiscovery.sh"

    # Gate 6.10: Mesh hello protocol contract (SRR6.46.6). Static/no-Cargo gate
    # covering the bounded hello request/response/error schemas and fixtures.
    run_stage "Mesh Hello Handshake E2E (SRR6.46.6)" "./scripts/e2e_overhaul/mesh_hello_handshake.sh"

    # Gate 6.11: Mesh hello responder lifecycle contract (SRR6.46.12). Static
    # no-Cargo gate covering daemon/status wiring, degraded fixtures, and audit names.
    run_stage "Mesh Hello Responder Lifecycle E2E (SRR6.46.12)" "./scripts/e2e_overhaul/hello_responder_lifecycle.sh"

    # Gate 7: Advanced End-to-End
    run_stage "Advanced E2E Scripts" "./scripts/e2e_advanced.sh"

    # Gate 8: Boundary Migration
    run_stage "Boundary Migration Scripts" "./scripts/e2e_boundary_migration.sh"

    # Gate 8.5: ee doctor safety harness (bd-21joy)
    # Wraps verify-undo.sh, verify-idempotence.sh, verify-crash-recovery.sh,
    # verify-concurrency.sh, verify-metamorphic.sh against the per-FM
    # fixture suite under tests/doctor_fixtures/ (owned by bd-2oh15).
    # Advisory while fixtures or sub-scripts are missing; set
    # EE_SAFETY_HARNESS_STRICT=1 to fail closed.
    run_stage "ee doctor Safety Harness (bd-21joy)" "./scripts/run-safety-harness.sh"
else
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Agent Ergonomics E2E (F1-F5) (ci-smoke)\n"
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Overhaul Integration E2E (J4) (ci-smoke)\n"
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Swarm Next-Action Recommendation Cards E2E (bd-3vwx0.6) (ci-smoke)\n"
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Graph Determinism E2E (F4.a) (ci-smoke)\n"
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Fake Tailscale Harness E2E (SRR6.46.10) (ci-smoke)\n"
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Tailscale Local Probe E2E (SRR6.46.1) (ci-smoke)\n"
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Tailscale Peer Autodiscovery E2E (SRR6.46.2) (ci-smoke)\n"
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Mesh Hello Handshake E2E (SRR6.46.6) (ci-smoke)\n"
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Mesh Hello Responder Lifecycle E2E (SRR6.46.12) (ci-smoke)\n"
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Advanced E2E Scripts (ci-smoke)\n"
    STAGE_RESULTS="${STAGE_RESULTS}SKIP Boundary Migration Scripts (ci-smoke)\n"
    STAGE_RESULTS="${STAGE_RESULTS}SKIP ee doctor Safety Harness (bd-21joy) (ci-smoke)\n"
fi

# Gate 8.8: Pack-quality eval regression sweep. Optional because it validates
# committed report artifacts and intended eval thresholds after feature slices.
if [ "$INCLUDE_EVAL" = "true" ]; then
    run_stage "Eval Regression (bd-bife.18)" "./scripts/eval_regression.sh"
fi

# Gate 9: Performance Benchmarks (optional, gated behind --include-bench)
if [ "$INCLUDE_BENCH" = "true" ]; then
    run_stage "Performance Benchmarks" "./scripts/bench_perf_regression.sh --check-regression"
fi

TOTAL_END=$(date +%s)
TOTAL_DURATION=$((TOTAL_END - TOTAL_START))

echo "=== All verification stages passed ==="
echo ""
echo "Summary:"
printf "%b" "$STAGE_RESULTS"
echo ""
echo "Total time: ${TOTAL_DURATION}s"

if [ -n "$ARTIFACT_DIRS" ]; then
    echo ""
    echo "Artifact directories:"
    printf "%b" "$ARTIFACT_DIRS"
fi

echo ""
echo "Test tracing log paths:"
if [ -n "$TRACE_LOG_DIRS" ]; then
    printf "%b" "$TRACE_LOG_DIRS"
else
    echo "  none recorded"
fi

artifact_retention_summary

exit 0
