#!/usr/bin/env bash
# N4.4 - Determinism lint e2e driver.
#
# Runs the cheap Clippy disallowed-methods gate, the exemption audit, and the
# known-violations fixture harness. A structured lint_determinism event records
# counts needed by the N4.4 closeout evidence.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=scripts/lib/e2e_logger.sh
source "$REPO_ROOT/scripts/lib/e2e_logger.sh"

run_status=0
disallowed_methods_violations=0
ui_tests_passed=0
ui_tests_failed=0

run_cargo_gate() {
    local label="$1"
    shift

    if [ "${EE_LINT_DETERMINISM_USE_RCH:-0}" = "1" ]; then
        if "$REPO_ROOT/scripts/rch_verify.sh" \
            --bead-id bd-17c65.14.4.4 \
            --summary \
            --no-write \
            --project-root "$REPO_ROOT" \
            -- "$@"; then
            e2e_log_assert_eq "true" "true" "$label"
            return 0
        fi
    elif (cd "$REPO_ROOT" && "$@"); then
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
    cargo test --test determinism_exemption_audit -- --nocapture || true

if run_cargo_gate \
    "lint_determinism_known_violations_fixture" \
    cargo test --test determinism_lint_catches_known_violations -- --nocapture; then
    ui_tests_passed=$((ui_tests_passed + 1))
else
    ui_tests_failed=$((ui_tests_failed + 1))
fi

emit_lint_summary "$EXEMPTIONS_COUNT" "$EXEMPTIONS_WITH_JUSTIFICATION"

exit "$run_status"
