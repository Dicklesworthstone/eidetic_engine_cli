#!/usr/bin/env bash
# scripts/e2e_orphan_partition_selftest.sh — prove e2e_orphan_partition.sh
# actually discriminates, by planting one script per verdict class and
# checking each lands where it belongs.
#
# WHY THIS EXISTS AT ALL. The partition tool's whole output is a
# classification, and a classifier nobody has watched misclassify is
# decoration. In particular NO_ASSERTIONS has to be checked by planting a
# script that exits 0 while asserting nothing: that is precisely the shape
# (`0 pass, 0 fail`, `COVERAGE 0/0 skipped=19`, a test filter matching no
# tests) that has already been misread as green in this repo more than once.
# If that one class silently collapsed into PASS, the partition would report
# the comfortable answer and the bead's question would stay unanswered.
#
# Hermetic: plants everything under a temp root, never touches scripts/.
# Exit 0 all classes correct · 1 a misclassification · 64 cannot run.

set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PARTITION="$SCRIPT_DIR/e2e_orphan_partition.sh"

[ -x "$PARTITION" ] || { echo "selftest: $PARTITION missing" >&2; exit 64; }
command -v timeout >/dev/null 2>&1 || { echo "selftest: timeout(1) required" >&2; exit 64; }

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ee-orphan-selftest-XXXXXX")" || exit 64
PLANTED="$ROOT/scripts"
mkdir -p "$PLANTED/sub"

# A stand-in `ee` that satisfies ee_require_current_binary: the gate compares
# `ee --version` against Cargo.toml's version, and this self-test grades the
# CLASSIFIER, not the product.
SRC_VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$REPO_ROOT/Cargo.toml" | sed -n '1p')"
FAKE_EE="$ROOT/ee"
printf '#!/bin/sh\necho "ee %s"\n' "$SRC_VERSION" > "$FAKE_EE"
chmod +x "$FAKE_EE"

# PASS: harness summary, assertions ran, none failed.
cat > "$PLANTED/p_pass.sh" <<'EOF'
#!/usr/bin/env bash
echo "[harness] p_pass: 7 pass, 0 fail, 3 steps, 0 drops, 12ms -> PASS" >&2
exit 0
EOF
# PASS_UNHARNESSED: exit 0, no harness summary at all.
cat > "$PLANTED/p_unharnessed.sh" <<'EOF'
#!/usr/bin/env bash
echo "did some work, printed no summary"
exit 0
EOF
# FAIL: nonzero exit with a harness summary naming failures.
cat > "$PLANTED/p_fail.sh" <<'EOF'
#!/usr/bin/env bash
echo "[harness] p_fail: 2 pass, 3 fail, 5 steps, 0 drops, 9ms -> FAIL" >&2
echo "  - the distinguishing failure text" >&2
exit 1
EOF
# NO_ASSERTIONS: exits 0 having asserted nothing. The class this file exists for.
cat > "$PLANTED/sub/p_zero.sh" <<'EOF'
#!/usr/bin/env bash
echo "[harness] p_zero: 0 pass, 0 fail, 0 steps, 0 drops, 1ms -> PASS" >&2
exit 0
EOF
# TIMEOUT: outlives the deadline.
cat > "$PLANTED/p_hang.sh" <<'EOF'
#!/usr/bin/env bash
sleep 30
EOF
chmod +x "$PLANTED"/*.sh "$PLANTED/sub"/*.sh

BASELINE="$ROOT/baseline.txt"
cat > "$BASELINE" <<'EOF'
# planted baseline
p_pass.sh
p_unharnessed.sh
p_fail.sh
sub/p_zero.sh
p_hang.sh
p_never_written.sh
EOF

OUT="$ROOT/out"
EE_BINARY="$FAKE_EE" EE_ORPHAN_OUT="$OUT" \
    "$PARTITION" --baseline "$BASELINE" --scripts-root "$PLANTED" --timeout 2 \
    > "$ROOT/run.stdout" 2> "$ROOT/run.stderr"
run_rc=$?

CENSUS="$OUT/partition.tsv"
if [ ! -f "$CENSUS" ]; then
    echo "selftest: no census at $CENSUS (partition rc=$run_rc); stderr tail:" >&2
    tail -20 "$ROOT/run.stderr" >&2
    exit 1
fi

FAILURES=0
expect() {
    local script="$1" want="$2" got
    got="$(awk -F'\t' -v s="$script" '$2 == s {print $1}' "$CENSUS" | tail -1)"
    if [ "$got" = "$want" ]; then
        printf 'selftest: ok   %-22s -> %s\n' "$script" "$got"
    else
        printf 'selftest: WRONG %-22s -> got %s, want %s\n' "$script" "${got:-<absent>}" "$want" >&2
        FAILURES=$((FAILURES + 1))
    fi
}

expect p_pass.sh          PASS
expect p_unharnessed.sh   PASS_UNHARNESSED
expect p_fail.sh          FAIL
expect sub/p_zero.sh      NO_ASSERTIONS
expect p_hang.sh          TIMEOUT
expect p_never_written.sh ABSENT

# The measurement-not-a-gate contract: a partition containing failures still
# exits 0, because a red partition is a finding and must not be confused with
# a broken harness.
if [ "$run_rc" -eq 0 ]; then
    echo "selftest: ok   partition exits 0 despite FAIL/TIMEOUT members"
else
    echo "selftest: WRONG partition exited $run_rc; a red partition is a finding, not a harness error" >&2
    FAILURES=$((FAILURES + 1))
fi

# The failing script's own output must reach the operator, not be swallowed.
if grep -qa 'the distinguishing failure text' "$ROOT/run.stderr"; then
    echo "selftest: ok   failing script's output reaches stderr"
else
    echo "selftest: WRONG failing script's output was discarded" >&2
    FAILURES=$((FAILURES + 1))
fi

if [ "$FAILURES" -ne 0 ]; then
    echo "selftest: $FAILURES classification error(s); artifacts kept at $ROOT" >&2
    exit 1
fi
# The temp root is left in place deliberately. AGENTS.md forbids `rm -rf`
# outright, and a recursive delete built from an interpolated path is exactly
# the footgun scripts/lib/e2e_harness.sh documents: with its variables unset,
# its own cleanup pattern degenerated to `/*` and matched every absolute path.
# mktemp dirs are small and the OS reaps them; an unguarded delete is not worth
# the tidiness.
echo "selftest: all 8 checks passed (artifacts under $ROOT)"
exit 0
