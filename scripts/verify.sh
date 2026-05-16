#!/bin/bash
set -euo pipefail

# EE-TST-LP4P-GAP-001 / EE-TST-LP4P-GAP-004: Central Verification Runner
#
# This script orchestrates the readiness gates for Eidetic Engine (ee).
# It executes standard tests, forbidden dependency checks, and the
# complex E2E/boundary migration pipelines.
#
# Usage:
#   ./scripts/verify.sh                # Run all gates
#   ./scripts/verify.sh --include-bench # Include performance benchmarks
#   ./scripts/verify.sh --help         # Show this help
#
# Gates (in order):
#   1. Forbidden Dependencies  - cargo tree audit for banned crates
#   2. Closure Linter          - prevent abstention-as-implementation closure
#   3. Snapshot Proposal Guard - block unreviewed tracked insta proposals
#   4. Untracked Work Audit    - advisory Beads FILE SURFACE coverage for dirty paths
#   4.5. Bridge Staleness      - advisory signal when CLOSE_THE_GAP_PLAN needs refresh
#   5. Vision Coverage         - report documented implemented/stubbed/missing surfaces
#   5.5. Proof Verification    - advisory Lean4/TLA+ proof artifact checks
#   6. Unit/Contract/Golden    - cargo test --workspace --lib --bins --tests --examples
#   6. Basic E2E               - scripts/e2e_test.sh
#   6.5. Overhaul Integration  - scripts/e2e_overhaul.sh  (gated by VERIFY_OVERHAUL)
#   6.6. Fake Tailscale Harness - deterministic SRR6.46 fake tailnet self-test
#   7. Advanced E2E            - scripts/e2e_advanced.sh
#   8. Boundary Migration      - scripts/e2e_boundary_migration.sh
#   9. Benchmarks (optional)   - scripts/bench_perf_regression.sh --check-regression
#
# Exit codes match AGENTS.md conventions (0=success, 1=usage, 3=storage, etc.)
# Artifacts are written to /tmp/ee-e2e-*/artifacts by E2E scripts.

INCLUDE_BENCH=false
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
            sed -n '3,27p' "$0" | sed 's/^# //' | sed 's/^#//'
            exit 0
            ;;
        --include-bench)
            INCLUDE_BENCH=true
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            exit 1
            ;;
    esac
done

echo "=== EE Verification Runner ==="
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

# Gate 1: Check Forbidden Dependencies
run_stage "Forbidden Dependencies" "./scripts/check-forbidden-deps.sh"

# Gate 2: Closure Discipline
run_stage "Closure Linter" "with_beads_read_locks ./scripts/closure-lint.sh --audit --json"

# Gate 2.5: Drift Guard (ensures red gates have tracking beads)
run_stage "Verification Drift Guard" "with_beads_read_locks ./scripts/verification-drift-guard.sh --json"

# Gate 3: Snapshot Proposal Guard
run_stage "Snapshot Proposal Guard" "snapshot_proposal_guard"

# Gate 3.5: Advisory dirty-work ownership coverage. This remains advisory while
# multi-agent sessions routinely carry unrelated in-flight changes.
run_stage "Untracked Work Audit (advisory)" "with_beads_read_locks ./scripts/untracked-work-audit.sh"

# Gate 3.6: Advisory bridge-plan staleness. This always exits 0 and writes
# .bridge-staleness-report.json so the trailing verify summary includes whether
# Part II appears stale enough to plan the next bridge.
run_stage "Bridge Staleness Advisory" "with_beads_read_locks ./scripts/bridge-staleness.sh --quiet"

# Gate 4: Strategic Vision Coverage
run_stage "Vision Coverage" "with_beads_read_locks sh ./scripts/vision-coverage.sh --json"

# Gate 4.5: Mechanized proof artifacts. Missing Lean4/TLA+ tools degrade
# inside the driver instead of blocking the default readiness gate.
run_stage "Proof Verification (bd-nnfq4)" "./scripts/e2e_overhaul/proof_verify.sh"

# Gate 5: Core Cargo Tests (Contracts, Logic, Golden). Benchmarks are
# deliberately excluded here and run only through the explicit benchmark gate.
run_stage "Unit, Contract, and Golden Tests" "cargo test --workspace --lib --bins --tests --examples"

# Gate 6: Basic End-to-End
run_stage "Basic E2E Scripts" "./scripts/e2e_test.sh"

# Gate 6.5: Overhaul Integration (J4). Gated behind VERIFY_OVERHAUL=1
# until enough implementation beads ship to make the suite reliably
# pass across CI. The driver itself respects VERIFY_OVERHAUL=0 and
# exits 0 without running, so this stage stays fast in default CI.
run_stage "Overhaul Integration E2E (J4)" "./scripts/e2e_overhaul.sh"

# Gate 6.6: Graph determinism harness (F4.a). This is separate from the J4
# epic registry because it tracks the GraphAccretion surfaces while they are
# landing incrementally.
run_stage "Graph Determinism E2E (F4.a)" "./scripts/e2e_overhaul/graph_determinism.sh"

# Gate 6.7: Fake Tailscale harness (SRR6.46.10). Later SRR6.46 auto-enrollment
# e2e scripts import this library, so this self-test runs before those surfaces.
run_stage "Fake Tailscale Harness E2E (SRR6.46.10)" "./scripts/e2e_overhaul/lib/test_fake_tailscale.sh"

# Gate 7: Advanced End-to-End
run_stage "Advanced E2E Scripts" "./scripts/e2e_advanced.sh"

# Gate 8: Boundary Migration
run_stage "Boundary Migration Scripts" "./scripts/e2e_boundary_migration.sh"

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
