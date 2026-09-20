#!/usr/bin/env bash
# N4.4 - Determinism lint e2e driver.
#
# Runs the cheap Clippy disallowed-methods gate, the exemption audit, and the
# known-violations fixture harness only when explicitly routed through RCH.
# Default execution logs skipped gates and never falls back to local Cargo.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=scripts/lib/e2e_logger.sh
source "$REPO_ROOT/scripts/lib/e2e_logger.sh"

run_status=0
disallowed_methods_violations=0
ui_tests_passed=0
ui_tests_failed=0
last_cargo_gate_skipped=0
gates_executed=0
gates_skipped=0

run_cargo_gate() {
    local label="$1"
    shift

    last_cargo_gate_skipped=0

    if [ "${EE_LINT_DETERMINISM_USE_RCH:-0}" != "1" ]; then
        # bd-r8p6j: this used to also call
        #     e2e_log_assert_eq "skipped" "skipped" "$label.remote_required"
        # which compares a constant with itself and so always incremented the
        # pass counter. This script already knew better -- last_cargo_gate_skipped
        # exists precisely so a skipped gate is not counted as a passed UI test
        # (:130) -- and then recorded a passing assertion anyway. The knowledge
        # was present and the report contradicted it.
        #
        # bd-gyn1a: the harness now has a verb for this, so the knowledge also
        # reaches the JSONL event stream instead of living only in these two
        # private counters. Emitting nothing here was still a gap: a gate that
        # vanishes from the log cannot be told apart from one that was deleted.
        e2e_log_skip "$label.remote_required" \
            "EE_LINT_DETERMINISM_USE_RCH is not 1; this gate requires remote execution and was not run"
        last_cargo_gate_skipped=1
        gates_skipped=$((gates_skipped + 1))
        return 0
    fi

    gates_executed=$((gates_executed + 1))

    if RCH_REQUIRE_REMOTE=1 "$REPO_ROOT/scripts/rch_verify.sh" \
        --bead-id bd-17c65.14.4.4 \
        --summary \
        --no-write \
        --project-root "$REPO_ROOT" \
        -- "$@"; then
        e2e_log_assert_eq "true" "true" "$label"
        return 0
    fi

    e2e_log_assert_eq "failed" "passed" "$label"
    run_status=1
    return 1
}

summarize_exemptions() {
    python3 - "$REPO_ROOT" <<'PY'
import os
import sys

repo_root = sys.argv[1]
roots = [os.path.join(repo_root, "src"), os.path.join(repo_root, "tests")]
exemption_lines = {
    "#[allow(clippy::disallowed_methods)]",
    "#![allow(clippy::disallowed_methods)]",
    "#[expect(clippy::disallowed_methods)]",
    "#![expect(clippy::disallowed_methods)]",
}
justification_markers = ("why:", "because", "justification:", "determinism:")

total = 0
justified = 0
for root in roots:
    if not os.path.isdir(root):
        continue
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames.sort()
        for filename in sorted(filenames):
            if not filename.endswith(".rs"):
                continue
            path = os.path.join(dirpath, filename)
            try:
                with open(path, encoding="utf-8") as handle:
                    lines = handle.read().splitlines()
            except OSError:
                continue
            for index, line in enumerate(lines):
                if line.lstrip() not in exemption_lines:
                    continue
                total += 1
                window = lines[index : min(index + 4, len(lines))]
                if any(
                    candidate.lower().lstrip().startswith("//")
                    and any(marker in candidate.lower() for marker in justification_markers)
                    for candidate in window
                ):
                    justified += 1

print(f"{total} {justified}")
PY
}

emit_lint_summary() {
    local exemptions_count="$1"
    local exemptions_with_justification="$2"
    _e2e_emit_event "lint_determinism" \
        "disallowed_methods_violations" "$disallowed_methods_violations" \
        "exemptions_count" "$exemptions_count" \
        "exemptions_with_justification" "$exemptions_with_justification" \
        "ui_tests_passed" "$ui_tests_passed" \
        "ui_tests_failed" "$ui_tests_failed"
}

e2e_log_start "lint_determinism"
trap 'e2e_log_end' EXIT

read -r EXEMPTIONS_COUNT EXEMPTIONS_WITH_JUSTIFICATION < <(summarize_exemptions)
if [ "${EE_LINT_DETERMINISM_COUNTS_ONLY:-0}" = "1" ]; then
    printf 'exemptions_count=%s exemptions_with_justification=%s\n' \
        "$EXEMPTIONS_COUNT" "$EXEMPTIONS_WITH_JUSTIFICATION"
    exit 0
fi

if ! run_cargo_gate \
    "lint_determinism_clippy_disallowed_methods" \
    cargo clippy --all-targets -- -D clippy::disallowed_methods; then
    disallowed_methods_violations=1
fi

run_cargo_gate \
    "lint_determinism_exemption_audit" \
    cargo test --test integration_a_d determinism_exemption_audit:: -- --nocapture || true

if run_cargo_gate \
    "lint_determinism_known_violations_fixture" \
    cargo test --test integration_a_d determinism_lint_catches_known_violations:: -- --nocapture; then
    if [ "$last_cargo_gate_skipped" -eq 0 ]; then
        ui_tests_passed=$((ui_tests_passed + 1))
    fi
else
    ui_tests_failed=$((ui_tests_failed + 1))
fi

emit_lint_summary "$EXEMPTIONS_COUNT" "$EXEMPTIONS_WITH_JUSTIFICATION"

# bd-r8p6j: NON-VACUITY GUARD. This script runs clippy --all-targets and two
# integration targets; none of that completes in one second. When every gate is
# skipped it previously exited 0 having executed no cargo at all, which would
# make it a permanently green stage if it were ever wired into verify.sh.
if [ "$gates_executed" -eq 0 ]; then
    printf 'lint_determinism: REFUSING TO PASS. %d cargo gate(s) skipped, 0 executed.\n' \
        "$gates_skipped" >&2
    printf '  Every gate here requires the remote lane. Set EE_LINT_DETERMINISM_USE_RCH=1 to run them.\n' >&2
    printf '  Exiting non-zero on purpose: a run that executed nothing must not be reported as a pass (bd-r8p6j).\n' >&2
    exit 2
fi

exit "$run_status"
