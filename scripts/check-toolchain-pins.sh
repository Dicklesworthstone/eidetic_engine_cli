#!/usr/bin/env bash
# bd-niay4 — every workflow that installs Rust must pin the repo's toolchain.
#
# WHY THIS EXISTS. rust-toolchain.toml pins `channel = "nightly-2026-08-31"` and
# its own comment states the hazard: a floating nightly means the same commit can
# build clean on one host and ICE on another, and that failure "presents as a
# code bug and gets debugged as one". Nineteen of forty-one workflow files
# defeated that pin anyway, including BOTH core gates, because of a distinction
# that is invisible at a glance:
#
#     uses: dtolnay/rust-toolchain@nightly      <- `@nightly` is the ACTION REF
#     with:
#       toolchain: nightly-2026-08-31           <- THIS is the toolchain
#
# Without the input, the action installs a floating nightly and exports
# RUSTUP_TOOLCHAIN, which OVERRIDES rust-toolchain.toml. So the repo's pin is
# silently defeated and every gate reports on a toolchain nobody chose.
#
# THE PREDICATE IS PER INVOCATION, NOT PER FILE, and that is the whole point.
# ci.yml contained FIVE installs and, before f0eb1c578/84236ce90, zero pins. A
# file-level "does this file mention a pin?" test would have passed it the
# moment one of the five was fixed, while four kept floating. So this counts
# INVOCATIONS and requires an equal number of correctly-dated inputs.
#
# IT PRINTS ITS DENOMINATOR AND ITS PREDICATE ON EVERY RUN, UNCONDITIONALLY.
# This bead produced three mutually incomparable censuses in one night -- 15 of
# 35, 19 of 41, 26 of 33 -- not because anyone miscounted, but because each used
# a different predicate and a different denominator and neither was printed
# beside the number. A guard that states both every run cannot do that. The
# separate failure this avoids is a check that emits its count only into a JSON
# report nobody opens, which is how closure-lint went quiet in CI.
#
# THIS SCRIPT NEVER WRITES. It reads workflows and reports. AGENTS.md forbids
# scripts that modify files in this repo; a checker is not one, and that
# distinction is the reason this is a script at all.
#
# Usage:
#   scripts/check-toolchain-pins.sh              audit the repo
#   scripts/check-toolchain-pins.sh --self-test  prove every failure direction
#
# Exit: 0 clean, 1 gate failure, 2 self-test failure, 3 environment error.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The action whose ref is mistakable for a toolchain. Kept as a variable so the
# self-test can assert the matcher, not a hardcoded string in two places.
TOOLCHAIN_ACTION='dtolnay/rust-toolchain@'

# Read the single source of truth. An unreadable pin is an ENVIRONMENT error
# (exit 3), never a pass: a guard that cannot find what it compares against must
# not report clean.
read_channel() {
    local toml="$1/rust-toolchain.toml"
    [ -f "$toml" ] || return 1
    local ch
    ch="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$toml" | head -1)"
    [ -n "$ch" ] || return 1
    printf '%s' "$ch"
}

# Per file: how many Rust toolchain INVOCATIONS, and how many correctly-dated
# `toolchain:` inputs. Prints "<invocations> <pins> <path>".
#
# Note both halves are counted over the same file with the same tool. An earlier
# census compared a grep of the action ref against a grep of the input and got a
# number that meant nothing, because the ref appears in correctly-pinned files
# too -- the ref is not the defect, the MISSING INPUT is.
count_file() {
    local file="$1" channel="$2" invocations pins
    invocations="$(grep -c -- "$TOOLCHAIN_ACTION" "$file" 2>/dev/null || true)"
    pins="$(grep -cE "^[[:space:]]*toolchain:[[:space:]]*${channel}[[:space:]]*$" "$file" 2>/dev/null || true)"
    printf '%s %s %s' "${invocations:-0}" "${pins:-0}" "$file"
}

audit() {
    local root="$1" channel="$2"
    local dir="$root/.github/workflows"
    local total_files=0 total_invocations=0 total_pins=0 bad=0
    local -a offenders=()
    local -A unpinned_by_file=()

    if [ ! -d "$dir" ]; then
        echo "[toolchain-pins] no .github/workflows under $root" >&2
        return 3
    fi

    local file
    # `find` rather than a glob: an unmatched zsh/bash glob behaves differently
    # across shells and an empty expansion here would read as "nothing to check".
    while IFS= read -r file; do
        total_files=$((total_files + 1))
        local line inv pin
        line="$(count_file "$file" "$channel")"
        inv="${line%% *}"
        pin="$(printf '%s' "$line" | cut -d' ' -f2)"
        total_invocations=$((total_invocations + inv))
        total_pins=$((total_pins + pin))
        if [ "$inv" -gt 0 ] && [ "$pin" -lt "$inv" ]; then
            local base_name; base_name="$(basename "$file")"
            offenders+=("$base_name: $inv invocation(s), $pin pinned")
            unpinned_by_file["$base_name"]=$((inv - pin))
            bad=$((bad + 1))
        fi
    done < <(find "$dir" -maxdepth 1 -type f \( -name '*.yml' -o -name '*.yaml' \) | sort)

    # PRINTED UNCONDITIONALLY, pass or fail. The predicate first, so the number
    # is never quotable without the rule that produced it.
    echo "[toolchain-pins] predicate: every '${TOOLCHAIN_ACTION}' invocation must be accompanied by 'toolchain: ${channel}' in the same file"
    echo "[toolchain-pins] denominator: ${total_files} workflow file(s); ${total_invocations} toolchain invocation(s); ${total_pins} pinned; ${bad} file(s) under-pinned"

    # RATCHET, not a hard fail. See scripts/toolchain-pins-baseline.txt for why:
    # 31 pre-existing unpinned invocations across 25 files cannot be fixed by
    # this lane, and a gate that fails on all of them could never be turned on.
    # A row may only SHRINK, and both directions fail -- a stale row decays into
    # a permanent allowlist.
    local baseline="$root/scripts/toolchain-pins-baseline.txt"
    local -A allowed=()
    if [ -f "$baseline" ]; then
        local bcount bfile
        while IFS=$'\t' read -r bcount bfile; do
            case "$bcount" in ''|'#'*) continue ;; esac
            [ -n "$bfile" ] && allowed["$bfile"]="$bcount"
        done < "$baseline"
    fi

    local violations=0
    local -a problems=()
    local entry name unpinned base
    for entry in "${offenders[@]}"; do
        name="${entry%%:*}"
        unpinned="${unpinned_by_file[$name]}"
        base="${allowed[$name]-}"
        if [ -z "$base" ]; then
            problems+=("$name: $unpinned unpinned invocation(s), NO baseline row — new unpinned debt")
            violations=$((violations + 1))
        elif [ "$unpinned" -gt "$base" ]; then
            problems+=("$name: $unpinned unpinned, baseline allows $base — rose by $((unpinned - base))")
            violations=$((violations + 1))
        elif [ "$unpinned" -lt "$base" ]; then
            problems+=("$name: $unpinned unpinned, baseline still says $base — good, but lower the row")
            violations=$((violations + 1))
        fi
    done
    # A baselined file that no longer appears at all must also drop its row.
    local bname
    for bname in "${!allowed[@]}"; do
        if [ -z "${unpinned_by_file[$bname]-}" ]; then
            problems+=("$bname: fully pinned or gone, but still has a baseline row — remove it")
            violations=$((violations + 1))
        fi
    done

    echo "[toolchain-pins] baseline: ${#allowed[@]} file(s) carry accepted unpinned debt (shrink-only)"

    if [ "$violations" -ne 0 ]; then
        echo "[toolchain-pins] FAIL — toolchain pinning moved the wrong way:" >&2
        local p
        for p in "${problems[@]}"; do echo "    $p" >&2; done
        # bd-niay4: THE REMEDIATION MUST NAME BOTH HALVES. This message used to
        # say only "add toolchain: <channel> to the with: block". Following it
        # literally on a job that also sets a floating RUSTUP_TOOLCHAIN pins the
        # install and leaves the INVOCATION floating, which is d315cdc7f: the
        # components land on the dated toolchain and the bare rustfmt/cargo runs
        # on the floating one, which does not have them. A gate whose advice
        # causes a regression is worse than a gate with no advice, because the
        # fixer has every reason to trust it.
        echo "[toolchain-pins] fix: PIN BOTH HALVES, OR NEITHER -- in the same commit:" >&2
        echo "[toolchain-pins]   1. 'toolchain: ${channel}' in the action's with: block, AND" >&2
        echo "[toolchain-pins]   2. any job-level 'env: RUSTUP_TOOLCHAIN:' set to ${channel} too." >&2
        echo "[toolchain-pins] RUSTUP_TOOLCHAIN overrides the action AT INVOCATION TIME, so pinning" >&2
        echo "[toolchain-pins] only the with: block installs components onto ${channel} and then runs" >&2
        echo "[toolchain-pins] a bare rustfmt/cargo on floating nightly, which does not have them:" >&2
        echo "[toolchain-pins]   error: 'rustfmt' is not installed for the toolchain 'nightly-...'" >&2
        echo "[toolchain-pins] Working examples already in this repo: ask-evidence-20260918.yml," >&2
        echo "[toolchain-pins] mcp-ask-20260918.yml, resume-coherence-20260919.yml." >&2
        echo "[toolchain-pins] '@nightly' in the uses: line is the ACTION REF and does not pin anything." >&2
        echo "[toolchain-pins] do NOT add a baseline row to silence a new finding." >&2
        return 1
    fi
    if [ "$bad" -ne 0 ]; then
        echo "[toolchain-pins] OK — ${bad} file(s) under-pinned, all within the shrink-only baseline"
    else
        echo "[toolchain-pins] OK — every toolchain invocation is pinned to ${channel}"
    fi
    return 0
}

# --------------------------------------------------------------------------
# --self-test: a guard that cannot fail is the defect it removes. Every arm
# below is a DIRECTION this check must catch, exercised on fixtures outside the
# repo tree so a self-test can never depend on, or perturb, real workflows.
# --------------------------------------------------------------------------
self_test() {
    local tmp failures=0 rc
    tmp="$(mktemp -d)" || { echo "[toolchain-pins] self-test: no temp dir" >&2; return 2; }
    mkdir -p "$tmp/.github/workflows"
    printf 'channel = "nightly-2026-08-31"\n' > "$tmp/rust-toolchain.toml"
    local ch="nightly-2026-08-31"

    arm() { # name, expected_rc
        local name="$1" want="$2"
        audit "$tmp" "$ch" >/dev/null 2>&1; rc=$?
        if [ "$rc" -eq "$want" ]; then
            echo "  arm OK   $name (rc=$rc)"
        else
            echo "  ARM FAIL $name: expected rc=$want, got rc=$rc" >&2
            failures=$((failures + 1))
        fi
        rm -f "$tmp/.github/workflows/"*.yml
    }

    # 1 NEGATIVE CONTROL — a correctly pinned install must PASS. Paired with (2):
    #   a checker that fails everything also passes arm 2 and is just as useless.
    printf 'jobs:\n  a:\n    steps:\n      - uses: dtolnay/rust-toolchain@nightly\n        with:\n          toolchain: %s\n' "$ch" \
        > "$tmp/.github/workflows/good.yml"
    arm "pinned install passes" 0

    # 2 POSITIVE CONTROL — a bare install must FAIL.
    printf 'jobs:\n  a:\n    steps:\n      - uses: dtolnay/rust-toolchain@nightly\n' \
        > "$tmp/.github/workflows/bare.yml"
    arm "unpinned install fails" 1

    # 3 THE PER-INVOCATION ARM — the one a file-level predicate gets wrong, and
    #   the reason this script counts invocations. Two installs, ONE pinned.
    #   ci.yml was exactly this shape at 5 and 0, then 5 and 3.
    printf 'jobs:\n  a:\n    steps:\n      - uses: dtolnay/rust-toolchain@nightly\n        with:\n          toolchain: %s\n      - uses: dtolnay/rust-toolchain@nightly\n' "$ch" \
        > "$tmp/.github/workflows/partial.yml"
    arm "partially pinned file fails (2 invocations, 1 pin)" 1

    # 4 WRONG-DATE ARM — pinned, but not to the repo's channel. A pin to some
    #   other nightly is not compliance; it is a second floating toolchain.
    printf 'jobs:\n  a:\n    steps:\n      - uses: dtolnay/rust-toolchain@nightly\n        with:\n          toolchain: nightly-2026-01-01\n' \
        > "$tmp/.github/workflows/wrongdate.yml"
    arm "pin to a different channel fails" 1

    # 5 NO-TOOLCHAIN ARM — a workflow that installs no Rust is not an offender.
    #   Without this, tightening the predicate later could start failing the 8
    #   workflows that legitimately never touch Rust.
    printf 'jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v4\n' \
        > "$tmp/.github/workflows/norust.yml"
    arm "workflow without Rust is not an offender" 0

    # 6-8 THE RATCHET ARMS. The baseline is the part that can silently rot, so
    #     each of its three directions gets an arm. Without these the ratchet
    #     could accept anything and arms 1-5 would still pass.
    mkdir -p "$tmp/scripts"
    printf 'jobs:\n  a:\n    steps:\n      - uses: dtolnay/rust-toolchain@nightly\n      - uses: dtolnay/rust-toolchain@nightly\n' \
        > "$tmp/.github/workflows/debt.yml"

    printf '2\tdebt.yml\n' > "$tmp/scripts/toolchain-pins-baseline.txt"
    audit "$tmp" "$ch" >/dev/null 2>&1; rc=$?
    if [ "$rc" -eq 0 ]; then echo "  arm OK   debt equal to its baseline row passes"
    else echo "  ARM FAIL baselined debt rejected (rc=$rc)" >&2; failures=$((failures + 1)); fi

    printf '1\tdebt.yml\n' > "$tmp/scripts/toolchain-pins-baseline.txt"
    audit "$tmp" "$ch" >/dev/null 2>&1; rc=$?
    if [ "$rc" -eq 1 ]; then echo "  arm OK   debt ABOVE its baseline row fails (the 26th-file case)"
    else echo "  ARM FAIL debt rose above baseline and was accepted (rc=$rc)" >&2; failures=$((failures + 1)); fi

    printf '3\tdebt.yml\n' > "$tmp/scripts/toolchain-pins-baseline.txt"
    audit "$tmp" "$ch" >/dev/null 2>&1; rc=$?
    if [ "$rc" -eq 1 ]; then echo "  arm OK   debt BELOW a stale row fails (forces the row down)"
    else echo "  ARM FAIL stale baseline row accepted (rc=$rc)" >&2; failures=$((failures + 1)); fi
    rm -f "$tmp/.github/workflows/"*.yml "$tmp/scripts/toolchain-pins-baseline.txt"

    # 9 ENVIRONMENT ARM — no rust-toolchain.toml must be an ERROR, not a pass.
    #   This is the arm that proves the 0 in arm 1 means something: a script that
    #   always returned 0 would pass arms 1 and 5 and fail only here.
    local empty; empty="$(mktemp -d)"
    mkdir -p "$empty/.github/workflows"
    if read_channel "$empty" >/dev/null 2>&1; then
        echo "  ARM FAIL missing rust-toolchain.toml was accepted" >&2
        failures=$((failures + 1))
    else
        echo "  arm OK   missing rust-toolchain.toml is an environment error"
    fi

    echo "[toolchain-pins] self-test: $((9 - failures))/9 arms passed"
    [ "$failures" -eq 0 ] || return 2
    return 0
}

main() {
    case "${1:-}" in
        --self-test) self_test; exit $? ;;
        "") ;;
        *) echo "usage: $0 [--self-test]" >&2; exit 3 ;;
    esac

    local channel
    if ! channel="$(read_channel "$REPO_ROOT")"; then
        echo "[toolchain-pins] cannot read channel from rust-toolchain.toml — refusing to report clean" >&2
        exit 3
    fi
    audit "$REPO_ROOT" "$channel"
    exit $?
}

main "$@"
