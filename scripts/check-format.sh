#!/usr/bin/env bash
# bd-gq26a — local rustfmt drift detector.
#
# WHY THIS EXISTS. Nothing checks Rust formatting between an agent's editor and
# hosted CI. scripts/verify.sh contains zero `fmt` references and both git hook
# drop-in directories contain only 50-agent-mail.py. The sole detector is the
# hosted "CI Static" workflow, and on 2026-09-18 its last 100 runs were 0
# success / 46 cancelled / 53 failure — every concluded run failing on the same
# step, Format, for one unformatted file (tests/contracts/ask_native.rs, red
# from 7bf162b28 until 3eee39a94). One formatting drift held the repo's only
# working gate at zero for a day. After that single formatting commit the same
# query returned 23 success. That delta is what this script protects.
#
# THIS SCRIPT NEVER WRITES. It runs `rustfmt --check` and reports; it does not
# format, rewrite, or stage anything. AGENTS.md forbids scripts that modify code
# files in this repo, and a formatter that edits under a hook is exactly that.
#
# NOT WIRED INTO verify.sh, DELIBERATELY. The verify budget admits zero new
# stages: the non-benchmark p50s total exactly 600 against a <=600 ceiling and
# UNMEASURED_STAGE_ALLOWANCE sits at its down-only ratchet of 27. Only widening
# an existing stage is available and none is a plausible host.
#
# NOT INSTALLED AS A HOOK, DELIBERATELY. See the bead: .git/hooks is not
# versioned so an install helps one checkout; a misbehaving pre-commit hook
# blocks commits for every agent in this shared tree; and there is currently no
# working gate to verify the installation against. Installation belongs to
# whoever re-enables hosted CI, at that moment.
#
# USAGE
#   scripts/check-format.sh              check tracked .rs files
#   scripts/check-format.sh --staged     check only files staged for commit
#   scripts/check-format.sh --self-test  prove both arms plus the fail-open arm
#
# EXIT CODES — the whole contract is here
#   0  clean, OR inconclusive (fail-open). A warning is always printed when
#      inconclusive, so a 0 is never silent about which kind it is.
#   1  a real formatting diff was found, and the offending files are PRINTED.
#
# FAIL-OPEN IS THE POINT. Anything that is not a formatting diff — rustfmt
# missing, a timeout, a parse error, not a git repo — exits 0 with a warning.
# A local convenience check must never be the reason a commit cannot happen,
# and this repo has already lost time to a pre-commit hook that hung while
# holding index.lock.

# NOT `set -e`: fail-open means this script decides every exit itself, and -e
# would hand that decision to the first command that returns non-zero.
set -uo pipefail

EDITION="${CHECK_FORMAT_EDITION:-2024}"
TIMEOUT_SECS="${CHECK_FORMAT_TIMEOUT_SECS:-60}"

warn() { printf '[check-format] %s\n' "$*" >&2; }
note() { printf '[check-format] %s\n' "$*"; }

# Resolve a timeout binary. GNU coreutils `timeout` is `gtimeout` on macOS when
# installed via brew; if neither exists we run without one and say so, because
# claiming a hard timeout we do not have is worse than not having one.
resolve_timeout() {
    if command -v timeout >/dev/null 2>&1; then printf 'timeout'; return 0; fi
    if command -v gtimeout >/dev/null 2>&1; then printf 'gtimeout'; return 0; fi
    printf ''
}

# Run rustfmt --check over the given files and classify the outcome.
#   prints the diff summary on drift
#   returns 0 clean, 1 drift, 2 inconclusive
run_rustfmt_check() {
    local out rc timeout_bin
    if ! command -v rustfmt >/dev/null 2>&1; then
        warn "rustfmt not found on PATH — cannot check formatting, not blocking"
        return 2
    fi
    timeout_bin="$(resolve_timeout)"
    if [ -n "$timeout_bin" ]; then
        out="$("$timeout_bin" "$TIMEOUT_SECS" rustfmt --edition "$EDITION" --check "$@" 2>&1)"
        rc=$?
    else
        warn "no timeout binary (timeout/gtimeout) — running without a hard cap"
        out="$(rustfmt --edition "$EDITION" --check "$@" 2>&1)"
        rc=$?
    fi

    if [ "$rc" -eq 124 ]; then
        warn "rustfmt exceeded ${TIMEOUT_SECS}s — inconclusive, not blocking"
        return 2
    fi
    if [ "$rc" -eq 0 ]; then
        return 0
    fi
    # rustfmt exits non-zero both for a formatting diff and for a parse error.
    # Only the former is this script's business. Discriminating on the OUTPUT
    # rather than the exit code is deliberate: a syntax error mid-edit must not
    # be reported as formatting drift, and must not block.
    if printf '%s' "$out" | grep -q '^Diff in '; then
        printf '%s\n' "$out"
        return 1
    fi
    warn "rustfmt exited ${rc} without producing a diff — inconclusive, not blocking"
    if [ -n "$out" ]; then printf '%s\n' "$out" >&2; fi
    return 2
}

# --------------------------------------------------------------------------
# --self-test: a format checker that cannot fail is the defect it removes.
# Three arms, each with its own fixture, none of them in the repo tree.
# --------------------------------------------------------------------------
self_test() {
    local tmp failures=0
    tmp="$(mktemp -d)" || { warn "self-test could not make a temp dir"; return 1; }

    # ARM 1 — POSITIVE CONTROL: badly formatted source must be reported.
    printf 'fn main( ) {let x=1;println!("{}",x);}\n' > "$tmp/bad.rs"
    if run_rustfmt_check "$tmp/bad.rs" >/dev/null 2>&1; then
        warn "SELF-TEST FAIL arm1: unformatted source was NOT reported as drift"
        failures=$((failures + 1))
    else
        local rc1=$?
        if [ "$rc1" -eq 1 ]; then
            note "self-test arm1 OK: unformatted source reported as drift"
        else
            warn "SELF-TEST FAIL arm1: expected rc=1 (drift), got rc=${rc1}"
            failures=$((failures + 1))
        fi
    fi

    # ARM 2 — NEGATIVE CONTROL: correctly formatted source must be clean.
    # Paired with arm 1 deliberately: a checker that reports EVERYTHING as drift
    # also passes arm 1, and would be just as useless.
    printf 'fn main() {\n    let x = 1;\n    println!("{x}");\n}\n' > "$tmp/good.rs"
    run_rustfmt_check "$tmp/good.rs" >/dev/null 2>&1
    local rc2=$?
    if [ "$rc2" -eq 0 ]; then
        note "self-test arm2 OK: formatted source reported clean"
    else
        warn "SELF-TEST FAIL arm2: formatted source gave rc=${rc2}, expected 0"
        failures=$((failures + 1))
    fi

    # ARM 3 — FAIL-OPEN CONTROL: with no rustfmt reachable, the answer must be
    # "inconclusive, not blocking" and NOT a silent clean. This is the arm that
    # proves the 0 in arm 2 means something: without it, a script that always
    # returned 0 would pass arm 2 and arm 3 both.
    local rc3
    mkdir -p "$tmp/emptybin"
    ( PATH="$tmp/emptybin" run_rustfmt_check "$tmp/good.rs" >/dev/null 2>&1 )
    rc3=$?
    if [ "$rc3" -eq 2 ]; then
        note "self-test arm3 OK: missing rustfmt is inconclusive, not clean"
    else
        warn "SELF-TEST FAIL arm3: missing rustfmt gave rc=${rc3}, expected 2"
        failures=$((failures + 1))
    fi

    if [ "$failures" -ne 0 ]; then
        warn "SELF-TEST FAILED: ${failures} of 3 arms"
        return 1
    fi
    note "self-test: 3 of 3 arms passed"
    return 0
}

# bd-l6h3g. Rides here rather than becoming a verify.sh stage, because the
# verify budget admits zero new stages (non-benchmark p50s total exactly 600
# against a <=600 ceiling, UNMEASURED_STAGE_ALLOWANCE at its down-only ratchet
# of 27). It belongs with the format check for a substantive reason, not just a
# budgetary one: BOTH are questions about the POPULATION a tool examines.
# `cargo fmt --check` reported this tree clean while two drifted files sat in
# it, because cargo walks targets and those files were reachable from none.
# Same blind spot, and the formatter's version of it is the harmless one.
run_reachability() {
    local script="${0%/*}/lib/mod_reachability.py"
    if [ ! -f "$script" ]; then
        warn "mod_reachability.py not found — skipping reachability, not blocking"
        return 0
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        warn "python3 not found — skipping reachability, not blocking"
        return 0
    fi
    python3 "$script"
    local rc=$?
    case "$rc" in
        0) return 0 ;;
        1) return 1 ;;
        # 2 is the script's own "inconclusive": cargo missing, or its controls
        # disagreed with observed cargo behaviour. It already warned; we do not
        # convert that into a block.
        *) return 0 ;;
    esac
}

main() {
    case "${1:-}" in
        --self-test) self_test; exit $? ;;
        --reachability) run_reachability; exit $? ;;
    esac

    if ! command -v git >/dev/null 2>&1 || ! git rev-parse --show-toplevel >/dev/null 2>&1; then
        warn "not a git repository — nothing to check, not blocking"
        exit 0
    fi

    local files=()
    if [ "${1:-}" = "--staged" ]; then
        while IFS= read -r line; do
            [ -n "$line" ] && files+=("$line")
        done < <(git diff --cached --name-only --diff-filter=ACMR -- '*.rs')
    else
        while IFS= read -r line; do
            [ -n "$line" ] && files+=("$line")
        done < <(git ls-files -- '*.rs')
    fi

    if [ "${#files[@]}" -eq 0 ]; then
        note "no Rust files to check"
        exit 0
    fi

    # Only check files that still exist on disk: a staged rename or a path
    # deleted after staging would otherwise make rustfmt error and turn a clean
    # tree into an inconclusive warning on every run.
    local present=()
    local f
    for f in "${files[@]}"; do
        [ -f "$f" ] && present+=("$f")
    done
    if [ "${#present[@]}" -eq 0 ]; then
        note "no Rust files present on disk to check"
        exit 0
    fi

    run_rustfmt_check "${present[@]}"
    local rc=$?
    local failed=0
    case "$rc" in
        0) note "${#present[@]} file(s) checked, formatting clean" ;;
        1)
            warn "FORMATTING DRIFT in the files listed above."
            warn "Fix with: rustfmt --edition ${EDITION} <file>   (this script never writes)"
            failed=1
            ;;
        *) : ;;   # inconclusive; run_rustfmt_check already warned
    esac

    # Runs even when formatting failed: reporting one finding and stopping is
    # how a check reports one defect and never five.
    if ! run_reachability; then
        failed=1
    fi

    exit "$failed"
}

main "$@"
