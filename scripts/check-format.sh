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
#   scripts/check-format.sh              run the gate's own `cargo fmt --check`
#   scripts/check-format.sh --staged     accepted, NO-OP (see run_cargo_fmt_check)
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

# Read the pin from rust-toolchain.toml rather than hard-coding it, so this
# script cannot drift from the toolchain the gate actually uses. Comments are
# stripped first: the file's own preamble mentions `channel` in prose.
PINNED_TOOLCHAIN="${CHECK_FORMAT_TOOLCHAIN:-}"
if [ -z "$PINNED_TOOLCHAIN" ] && [ -f rust-toolchain.toml ]; then
    PINNED_TOOLCHAIN="$(grep -vE '^[[:space:]]*#' rust-toolchain.toml \
        | grep -E '^[[:space:]]*channel[[:space:]]*=' \
        | head -1 | sed -E 's/.*=[[:space:]]*"([^"]+)".*/\1/')"
fi

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

# Run the GATE'S OWN command over the crate and classify the outcome.
#   prints the diff summary on drift
#   returns 0 clean, 1 drift, 2 inconclusive
#
# WHY NOT ENUMERATE FILES OURSELVES. This script exists to PREDICT CI Static's
# Format step. A predictor that checks a DIFFERENT SET of files than the gate is
# not a stricter predictor, it is a broken one -- and this one was.
#
# MEASURED 2026-09-21 on a tree the gate called clean (`cargo fmt --check`
# exit 0), the previous `git ls-files -- '*.rs'` + per-file `rustfmt` path
# returned exit 1 and named four files:
#
#   src/cass/backfill_public_tests.rs   pulled in by `include!` (src/cass/import.rs:2435).
#                                       rustfmt follows `mod` and `#[path]`, NEVER
#                                       `include!`, so the gate never formats it. Its
#                                       body is indented because it is spliced inside a
#                                       module, and standalone rustfmt demands that
#                                       indentation be removed -- a diff that can never
#                                       be resolved.
#   src/mcp_ask.rs                      behind `#[cfg(feature = "mcp")]` (src/lib.rs:38),
#   src/mcp_ask_live_tests.rs           so `cargo fmt` under default features never
#                                       reaches either.
#   src/core/backup_workflow_recovery_tests.rs
#                                       tracked but reachable from no module declaration.
#
# All four are the GATE'S blind spots, and this script had the inverse coverage.
# Neither set contains the other, so the two tools disagreed permanently. The
# practical effect: the old path returned 1 on a clean tree AND 1 on a dirty one,
# so it could not distinguish the two states it exists to distinguish. Wired into
# a blocking hook it would have blocked every push in this repo, forever, over a
# file CI deliberately never examines.
#
# Delegating to `cargo fmt --check` fixes the scope by construction: same walker,
# same rustfmt.toml resolution, same answer. Measured at 6.5s for the whole crate
# with no build, which is why the old `--staged` narrowing bought nothing worth
# a scope mismatch.
run_cargo_fmt_check() {
    local out rc timeout_bin
    local -a fmt_cmd
    if ! command -v cargo >/dev/null 2>&1; then
        warn "cargo not found on PATH — cannot check formatting, not blocking"
        return 2
    fi

    # The pinned toolchain, because rustfmt OUTPUT DIFFERS BETWEEN NIGHTLIES.
    # `cargo +toolchain` fails where cargo is not the rustup shim (it is not on
    # the dev Macs here), so go through `rustup run`. If the pin is unavailable
    # we still check, and say which toolchain answered.
    fmt_cmd=(cargo fmt --check)
    if [ -n "$PINNED_TOOLCHAIN" ] \
        && command -v rustup >/dev/null 2>&1 \
        && rustup run "$PINNED_TOOLCHAIN" true >/dev/null 2>&1; then
        fmt_cmd=(rustup run "$PINNED_TOOLCHAIN" cargo fmt --check)
    else
        warn "pinned toolchain ${PINNED_TOOLCHAIN:-<unresolved>} unavailable — using default cargo fmt; verdict may differ from CI"
    fi

    timeout_bin="$(resolve_timeout)"
    if [ -n "$timeout_bin" ]; then
        out="$("$timeout_bin" "$TIMEOUT_SECS" "${fmt_cmd[@]}" 2>&1)"
        rc=$?
    else
        warn "no timeout binary (timeout/gtimeout) — running without a hard cap"
        out="$("${fmt_cmd[@]}" 2>&1)"
        rc=$?
    fi

    if [ "$rc" -eq 124 ]; then
        warn "cargo fmt exceeded ${TIMEOUT_SECS}s — inconclusive, not blocking"
        return 2
    fi
    if [ "$rc" -eq 0 ]; then
        return 0
    fi
    # Same discrimination as before, and for the same reason: a parse error
    # mid-edit is not formatting drift and must not block.
    if printf '%s' "$out" | grep -q '^Diff in '; then
        printf '%s\n' "$out"
        return 1
    fi
    warn "cargo fmt exited ${rc} without producing a diff — inconclusive, not blocking"
    if [ -n "$out" ]; then printf '%s\n' "$out" >&2; fi
    return 2
}

# Run rustfmt --check over the given files and classify the outcome.
#   prints the diff summary on drift
#   returns 0 clean, 1 drift, 2 inconclusive
#
# STILL USED BY --self-test, where it is CORRECT: those fixtures are standalone
# whole files in a temp dir, outside any module tree, so per-file rustfmt is
# exactly the right instrument. It is no longer used against the repo.
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
        # 2 is ENVIRONMENTAL: cargo missing or timed out, the tree was not
        # examined. Fail open; it already warned.
        2) return 0 ;;
        # 3 is THE INSTRUMENT REPORTING ITSELF BROKEN -- a self-validation
        # control disagreed with observed cargo behaviour. This must NOT fail
        # open. An earlier version of this function mapped 2 and 3 to the same
        # "not blocking" branch, which meant a resolver whose controls had
        # failed reported warned-but-green: a check that cannot tell you it is
        # broken. That is the defect this whole gate exists to find, and it was
        # sitting in the caller.
        3)
            warn "reachability self-validation FAILED — the check is broken, not the tree."
            return 1
            ;;
        *)
            warn "reachability returned unexpected status ${rc} — treating as a failure."
            return 1
            ;;
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

    # `--staged` is accepted and ignored, deliberately. It narrowed the file set,
    # and narrowing the file set is what made this script disagree with the gate.
    # `cargo fmt --check` is 6.5s for the whole crate, so there is nothing to buy.
    if [ "${1:-}" = "--staged" ]; then
        note "--staged is a no-op: the gate is whole-crate, and matching its scope is the point"
    fi

    run_cargo_fmt_check
    local rc=$?
    local failed=0
    case "$rc" in
        0) note "formatting clean (cargo fmt --check, toolchain ${PINNED_TOOLCHAIN:-default})" ;;
        1)
            warn "FORMATTING DRIFT in the files listed above."
            warn "Fix with: rustup run ${PINNED_TOOLCHAIN:-nightly} cargo fmt -- <file>   (this script never writes)"
            failed=1
            ;;
        *) : ;;   # inconclusive; run_cargo_fmt_check already warned
    esac

    # Runs even when formatting failed: reporting one finding and stopping is
    # how a check reports one defect and never five.
    if ! run_reachability; then
        failed=1
    fi

    exit "$failed"
}

main "$@"
