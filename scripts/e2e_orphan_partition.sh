#!/usr/bin/env bash
# scripts/e2e_orphan_partition.sh — produce the partition that
# bd-unreachable-e2e-scripts-u14sr names as its actual deliverable.
#
# That bead measured 52 orphaned e2e scripts of 209: written, committed, cited
# by closed beads as proof, and invoked by no gate. It is explicit that the
# first move is NOT to bulk-wire them:
#
#   "The right first move is to find out which of the 25 even PASS, because a
#    script that has not run since it was written is as likely to be broken as
#    to be green, and a red one is a different (and more urgent) finding than
#    an unwired one."
#
# This runs each one and classifies it. It is a MEASUREMENT TOOL, NOT A GATE:
# it exits 0 whenever the measurement completed, whatever the partition says,
# and nonzero only when the measurement could not be made. A red partition is
# the expected first result; if that exited nonzero, the finding would be
# indistinguishable from the harness being broken.
#
# ZERO OF THE 52 FAIL `bash -n`, so there is no cheap static shortcut to this
# answer (measured 2026-10-18 at 63ed35466, instrument validated both ways with
# a planted syntax error). The partition requires execution.
#
# Usage:
#   scripts/e2e_orphan_partition.sh [--timeout SEC] [--only SUBSTRING]
#
#   EE_BINARY / EE_BIN   binary under test; must match Cargo.toml's version
#   EE_ORPHAN_OUT        output directory (default: under TMPDIR)
#
# Exit: 0 measurement completed · 1 measurement impossible · 64 usage error.

set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BASELINE="$REPO_ROOT/tests/fixtures/e2e_invocation/orphan_baseline.txt"
# Baseline entries are paths relative to this root. Overridable only so the
# self-test can plant its own scripts outside the repo; production runs never
# pass it.
SCRIPTS_ROOT="$REPO_ROOT/scripts"

TIMEOUT_SEC="${EE_ORPHAN_TIMEOUT_SEC:-600}"
ONLY=""
while [ $# -gt 0 ]; do
    case "$1" in
    --timeout) TIMEOUT_SEC="${2:?--timeout needs a value}"; shift 2 ;;
    --only) ONLY="${2:?--only needs a value}"; shift 2 ;;
    # --baseline exists so this classifier can be exercised against planted
    # scripts of every verdict class. A partition tool nobody has watched
    # misclassify is decoration; see --self-test.
    --baseline) BASELINE="${2:?--baseline needs a value}"; shift 2 ;;
    --scripts-root) SCRIPTS_ROOT="${2:?--scripts-root needs a value}"; shift 2 ;;
    --self-test) exec "$SCRIPT_DIR/e2e_orphan_partition_selftest.sh" ;;
    -h | --help) sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "e2e-orphan-partition: unknown argument '$1'" >&2; exit 64 ;;
    esac
done

# shellcheck source=scripts/lib/ee_binary_resolution.sh
# shellcheck disable=SC1091
. "$REPO_ROOT/scripts/lib/ee_binary_resolution.sh"
EE_BIN="${EE_BINARY:-${EE_BIN:-$(ee_resolve_binary 2>/dev/null)}}"

# Refuse a stale or foreign binary rather than grading one. A partition taken
# against last month's `ee` would attribute the repo's drift to these scripts,
# which is the exact misreading this bead exists to stop.
if ! ee_require_current_binary "$EE_BIN" e2e-orphan-partition; then
    echo "e2e-orphan-partition: cannot measure without a current-source binary." >&2
    exit 1
fi

[ -f "$BASELINE" ] || { echo "e2e-orphan-partition: $BASELINE missing" >&2; exit 64; }
command -v timeout >/dev/null 2>&1 || { echo "e2e-orphan-partition: timeout(1) required" >&2; exit 64; }

OUT="${EE_ORPHAN_OUT:-${TMPDIR:-/tmp}/ee-orphan-partition-$$}"
mkdir -p "$OUT" || { echo "e2e-orphan-partition: cannot create $OUT" >&2; exit 1; }
CENSUS="$OUT/partition.tsv"
printf 'verdict\tscript\trc\telapsed_s\tpass\tfail\tdrops\tlog\n' > "$CENSUS"

RAN=0
declare -a MEMBERS_PASS=() MEMBERS_PASS_UNHARNESSED=() MEMBERS_FAIL=() \
    MEMBERS_NO_ASSERTIONS=() MEMBERS_TIMEOUT=() MEMBERS_ABSENT=()

while IFS= read -r rel; do
    case "$rel" in '' | '#'*) continue ;; esac
    [ -n "$ONLY" ] && case "$rel" in *"$ONLY"*) ;; *) continue ;; esac
    script="$SCRIPTS_ROOT/$rel"
    slug="$(printf '%s' "$rel" | tr '/' '_')"
    log="$OUT/$slug.log"

    if [ ! -f "$script" ]; then
        MEMBERS_ABSENT+=("$rel")
        printf 'ABSENT\t%s\t-\t-\t-\t-\t-\t-\n' "$rel" >> "$CENSUS"
        continue
    fi

    started=$(date +%s)
    # Combined capture, never discarded: the whole point of this bead's
    # sibling findings is that a run reporting THAT something failed while
    # discarding WHAT failed costs a fresh execution every time it fires.
    timeout "$TIMEOUT_SEC" env EE_BINARY="$EE_BIN" EE_BIN="$EE_BIN" \
        bash "$script" > "$log" 2>&1
    rc=$?
    elapsed=$(( $(date +%s) - started ))
    RAN=$((RAN + 1))

    # `[harness] <name>: N pass, M fail, S steps, D drops, Xms -> PASS|FAIL`
    summary="$(grep -a '^\[harness\] ' "$log" | tail -1)"
    p="-"; f="-"; d="-"
    if [ -n "$summary" ]; then
        p="$(printf '%s' "$summary" | sed -n 's/.*: \([0-9][0-9]*\) pass,.*/\1/p')"
        f="$(printf '%s' "$summary" | sed -n 's/.*, \([0-9][0-9]*\) fail,.*/\1/p')"
        d="$(printf '%s' "$summary" | sed -n 's/.*, \([0-9][0-9]*\) drops,.*/\1/p')"
    fi
    [ -n "$p" ] || p="-"; [ -n "$f" ] || f="-"; [ -n "$d" ] || d="-"

    if [ "$rc" -eq 124 ]; then
        verdict=TIMEOUT; MEMBERS_TIMEOUT+=("$rel")
    elif [ -n "$summary" ] && [ "$p" = "0" ] && [ "$f" = "0" ]; then
        # A ZERO IS NOT A PASS. `0 pass, 0 fail` means the script completed
        # having asserted nothing -- the same shape as a test filter matching
        # no tests, or a fixture suite reporting `COVERAGE 0/0 skipped=19`.
        # Both of those have already been mistaken for green in this repo.
        # It is reported as its own class so it can never be counted as one.
        verdict=NO_ASSERTIONS; MEMBERS_NO_ASSERTIONS+=("$rel")
    elif [ "$rc" -ne 0 ]; then
        verdict=FAIL; MEMBERS_FAIL+=("$rel")
    elif [ -n "$summary" ]; then
        verdict=PASS; MEMBERS_PASS+=("$rel")
    else
        # Exited 0 but printed no harness summary: the twelve standalone
        # scripts do not source e2e_harness.sh, so their silence is normal and
        # their green is simply weaker evidence. Kept separate from PASS rather
        # than merged into it, because "asserted things and they held" and
        # "exited 0" are different claims.
        verdict=PASS_UNHARNESSED; MEMBERS_PASS_UNHARNESSED+=("$rel")
    fi

    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$verdict" "$rel" "$rc" "$elapsed" "$p" "$f" "$d" "$log" >> "$CENSUS"
    printf 'e2e-orphan-partition: %-16s %-52s rc=%-3s %ss\n' \
        "$verdict" "$rel" "$rc" "$elapsed" >&2
    if [ "$verdict" != PASS ] && [ "$verdict" != PASS_UNHARNESSED ]; then
        tail -12 "$log" | sed "s|^|  [$rel] |" >&2
    fi
done < "$BASELINE"

emit_group() {
    local label="$1"; shift
    [ "$#" -gt 0 ] || return 0
    printf '\n%s (%d):\n' "$label" "$#" >&2
    printf '  %s\n' "$@" >&2
}

echo >&2
echo "=== e2e orphan partition ===" >&2
emit_group "PASS (harness summary, no failures)" "${MEMBERS_PASS[@]+"${MEMBERS_PASS[@]}"}"
emit_group "PASS_UNHARNESSED (exit 0, no summary)" "${MEMBERS_PASS_UNHARNESSED[@]+"${MEMBERS_PASS_UNHARNESSED[@]}"}"
emit_group "FAIL (already-regressed behaviour)" "${MEMBERS_FAIL[@]+"${MEMBERS_FAIL[@]}"}"
emit_group "NO_ASSERTIONS (ran, asserted nothing)" "${MEMBERS_NO_ASSERTIONS[@]+"${MEMBERS_NO_ASSERTIONS[@]}"}"
emit_group "TIMEOUT (>${TIMEOUT_SEC}s)" "${MEMBERS_TIMEOUT[@]+"${MEMBERS_TIMEOUT[@]}"}"
emit_group "ABSENT (baselined but not on disk)" "${MEMBERS_ABSENT[@]+"${MEMBERS_ABSENT[@]}"}"

printf '\ne2e-orphan-partition: ran=%d pass=%d pass_unharnessed=%d fail=%d no_assertions=%d timeout=%d absent=%d census=%s\n' \
    "$RAN" \
    "${#MEMBERS_PASS[@]}" "${#MEMBERS_PASS_UNHARNESSED[@]}" "${#MEMBERS_FAIL[@]}" \
    "${#MEMBERS_NO_ASSERTIONS[@]}" "${#MEMBERS_TIMEOUT[@]}" "${#MEMBERS_ABSENT[@]}" \
    "$CENSUS" >&2

# Empty-world guard. A run that executed nothing has measured nothing, and must
# not be reported as a completed partition.
if [ "$RAN" -eq 0 ]; then
    echo "e2e-orphan-partition: executed 0 scripts; a partition of nothing is not a partition" >&2
    exit 1
fi
exit 0
