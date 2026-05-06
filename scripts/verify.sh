#!/bin/sh
set -eu

# EE-TST-LP4P-GAP-001 / EE-TST-LP4P-GAP-004: Central Verification Runner
#
# This script orchestrates the readiness gates for Eidetic Engine (ee).
# It executes standard tests, forbidden dependency checks, and the
# complex E2E/boundary migration pipelines.
#
# Usage:
#   ./scripts/verify.sh          # Run all gates
#   ./scripts/verify.sh --help   # Show this help
#
# Gates (in order):
#   1. Forbidden Dependencies  - cargo tree audit for banned crates
#   2. Unit/Contract/Golden    - cargo test --workspace --all-targets
#   3. Basic E2E               - scripts/e2e_test.sh
#   4. Advanced E2E            - scripts/e2e_advanced.sh
#   5. Boundary Migration      - scripts/e2e_boundary_migration.sh
#
# Exit codes match AGENTS.md conventions (0=success, 1=usage, 3=storage, etc.)
# Artifacts are written to /tmp/ee-e2e-*/artifacts by E2E scripts.

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
    sed -n '3,20p' "$0" | sed 's/^# //' | sed 's/^#//'
    exit 0
fi

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

# Gate 2: Core Cargo Tests (Contracts, Logic, Golden)
run_stage "Unit, Contract, and Golden Tests" "cargo test --workspace --all-targets"

# Gate 3: Basic End-to-End
run_stage "Basic E2E Scripts" "./scripts/e2e_test.sh"

# Gate 4: Advanced End-to-End
run_stage "Advanced E2E Scripts" "./scripts/e2e_advanced.sh"

# Gate 5: Boundary Migration
run_stage "Boundary Migration Scripts" "./scripts/e2e_boundary_migration.sh"

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
