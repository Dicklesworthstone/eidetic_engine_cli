#!/bin/sh
set -eu

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
#   3. Vision Coverage         - report documented implemented/stubbed/missing surfaces
#   4. Unit/Contract/Golden    - cargo test --workspace --all-targets
#   5. Basic E2E               - scripts/e2e_test.sh
#   6. Advanced E2E            - scripts/e2e_advanced.sh
#   7. Boundary Migration      - scripts/e2e_boundary_migration.sh
#   8. Benchmarks (optional)   - scripts/bench.sh --check-regression
#
# Exit codes match AGENTS.md conventions (0=success, 1=usage, 3=storage, etc.)
# Artifacts are written to /tmp/ee-e2e-*/artifacts by E2E scripts.

INCLUDE_BENCH=false

for arg in "$@"; do
    case "$arg" in
        --help|-h)
            sed -n '3,21p' "$0" | sed 's/^# //' | sed 's/^#//'
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

ARTIFACT_DIRS=""
STAGE_RESULTS=""
TOTAL_START=$(date +%s)

run_stage() {
    local name="$1"
    local cmd="$2"
    echo "[*] Running: $name"
    echo "    $cmd"

    local start_time=$(date +%s)
    local output_file=$(mktemp)

    if eval "$cmd" 2>&1 | tee "$output_file"; then
        local end_time=$(date +%s)
        local duration=$((end_time - start_time))
        echo "[+] PASS: $name (${duration}s)"
        STAGE_RESULTS="${STAGE_RESULTS}PASS ${name} (${duration}s)\n"

        # Capture artifact paths from E2E output
        local artifacts=$(grep -o 'Artifacts:[[:space:]]*[^ ]*' "$output_file" | head -1 | sed 's/Artifacts:[[:space:]]*//' || true)
        if [ -n "$artifacts" ] && [ -d "$artifacts" ]; then
            ARTIFACT_DIRS="${ARTIFACT_DIRS}  ${name}: ${artifacts}\n"
        fi
        rm -f "$output_file"
        echo ""
    else
        local exit_code=$?
        local end_time=$(date +%s)
        local duration=$((end_time - start_time))
        echo "[-] FAIL: $name (Exit code: $exit_code, ${duration}s)"
        rm -f "$output_file"
        exit $exit_code
    fi
}

# Gate 1: Check Forbidden Dependencies
run_stage "Forbidden Dependencies" "./scripts/check-forbidden-deps.sh"

# Gate 2: Closure Discipline
run_stage "Closure Linter" "./scripts/closure-lint.sh --json"

# Gate 3: Strategic Vision Coverage
run_stage "Vision Coverage" "sh ./scripts/vision-coverage.sh --json"

# Gate 4: Core Cargo Tests (Contracts, Logic, Golden)
run_stage "Unit, Contract, and Golden Tests" "cargo test --workspace --all-targets"

# Gate 5: Basic End-to-End
run_stage "Basic E2E Scripts" "./scripts/e2e_test.sh"

# Gate 6: Advanced End-to-End
run_stage "Advanced E2E Scripts" "./scripts/e2e_advanced.sh"

# Gate 7: Boundary Migration
run_stage "Boundary Migration Scripts" "./scripts/e2e_boundary_migration.sh"

# Gate 8: Performance Benchmarks (optional, gated behind --include-bench)
if [ "$INCLUDE_BENCH" = "true" ]; then
    run_stage "Performance Benchmarks" "./scripts/bench.sh --check-regression"
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

exit 0
