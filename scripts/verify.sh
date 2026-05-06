#!/bin/sh
set -eu

# EE-TST-LP4P-GAP-001 / EE-TST-LP4P-GAP-004: Central Verification Runner
#
# This script orchestrates the readiness gates for Eidetic Engine (ee).
# It executes standard tests, forbidden dependency checks, and the
# complex E2E/boundary migration pipelines.

echo "=== EE Verification Runner ==="
echo ""

run_stage() {
    local name="$1"
    local cmd="$2"
    echo "[*] Running: $name"
    echo "    $cmd"
    
    local start_time=$(date +%s)
    if eval "$cmd"; then
        local end_time=$(date +%s)
        local duration=$((end_time - start_time))
        echo "[+] PASS: $name (${duration}s)"
        echo ""
    else
        local exit_code=$?
        local end_time=$(date +%s)
        local duration=$((end_time - start_time))
        echo "[-] FAIL: $name (Exit code: $exit_code, ${duration}s)"
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

echo "=== All verification stages passed ==="
exit 0
