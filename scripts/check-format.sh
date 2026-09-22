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
# Checks never format, rewrite, or stage project sources. --self-test writes
# temporary fixtures; only --install-pre-push writes a Git hook, on explicit
# request. AGENTS.md forbids scripts that rewrite project code under a hook.
#
# NOT WIRED INTO verify.sh, DELIBERATELY. The verify budget admits zero new
# stages: the non-benchmark p50s total exactly 600 against a <=600 ceiling and
# UNMEASURED_STAGE_ALLOWANCE sits at its down-only ratchet of 27. Only widening
# an existing stage is available and none is a plausible host.
#
# This is a whole-working-tree check, not a staged-blob check. A pre-push
# installation is per clone; see docs/testing-strategy.md. It does not change
# the index or hold index.lock. Unstaged formatting drift can block a push.
#
# --install-pre-push PRECONDITION: installation remains deferred until the
# operator who re-enables hosted CI explicitly authorizes it, with a working
# hosted gate available to verify it against. Approval to land this checker
# does not authorize installation. Hooks are unversioned and help only the
# checkout where installed. This tree is shared: the measured ~22-second,
# whole-tree check can let one agent's unstaged work block another's push.
# Moving the check from pre-commit to pre-push avoids index.lock contention;
# it does not remove that shared-tree hazard. See the decision on bd-gq26a.
#
# USAGE
#   scripts/check-format.sh              run CI's primary and include-only checks
#   scripts/check-format.sh --staged     accepted, NO-OP (see run_cargo_fmt_check)
#   scripts/check-format.sh --self-test  exercise real formatters and config lookup
#   scripts/check-format.sh --install-pre-push  DEFERRED; precondition above
#
# EXIT CODES — the whole contract is here
#   0  clean, OR inconclusive (fail-open). A warning is always printed when
#      inconclusive, so a 0 is never silent about which kind it is.
#   1  formatting drift, a reachability finding, or a failed include-only check.
#
# Missing tooling warns and fails open, as required by the original bead.
# This is not a CI pass. Existing reachability failure handling is preserved;
# the include-only check retains CI's nonzero environmental failure statuses.
# Installation refuses to replace an existing hook or an unknown dispatcher.

# NOT `set -e`: fail-open means this script decides every exit itself, and -e
# would hand that decision to the first command that returns non-zero.
set -uo pipefail

EDITION="${CHECK_FORMAT_EDITION:-2024}"
TIMEOUT_SECS="${CHECK_FORMAT_TIMEOUT_SECS:-60}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PINNED_TOOLCHAIN=""

warn() { printf '[check-format] %s\n' "$*" >&2; }
note() { printf '[check-format] %s\n' "$*"; }

resolve_toolchain() {
    # Parse the value, not a dated nightly mentioned in a comment. Do not let
    # CHECK_FORMAT_TOOLCHAIN or an ambient RUSTUP_TOOLCHAIN replace CI's pin.
    if ! PINNED_TOOLCHAIN="$(python3 - "$REPO_ROOT/rust-toolchain.toml" <<'PY'
import re
import sys
import tomllib

with open(sys.argv[1], "rb") as source:
    channel = tomllib.load(source)["toolchain"]["channel"]
if not isinstance(channel, str) or not re.fullmatch(r"nightly-\d{4}-\d{2}-\d{2}", channel):
    raise ValueError("expected a dated nightly toolchain")
print(channel)
PY
    )"; then
        warn "cannot resolve the pinned toolchain — inconclusive, not blocking"
        return 2
    fi
    if ! command -v rustup >/dev/null 2>&1 \
        || ! rustup run "$PINNED_TOOLCHAIN" rustfmt --version >/dev/null 2>&1; then
        warn "rustfmt unavailable under $PINNED_TOOLCHAIN — inconclusive, not blocking; no fallback toolchain used"
        return 2
    fi
    # Cargo permits RUSTFMT to replace the formatter. Pin that too, including
    # cargo calls made by the reachability/include-only population resolver.
    RUSTFMT="$(rustup which --toolchain "$PINNED_TOOLCHAIN" rustfmt)" || return 2
    export RUSTFMT
    export RUSTUP_TOOLCHAIN="$PINNED_TOOLCHAIN"
}

install_pre_push() {
    # Operator-only installation decision; see --install-pre-push precondition
    # above. The flag's availability is not permission to install in this tree.
    # Only this explicit opt-in writes, and only under Git's hooks directory.
    # Preserve Agent Mail's dispatcher and every other drop-in. A clone without
    # a dispatcher gets a standalone hook; an unknown hook needs manual review.
    python3 - "$REPO_ROOT" <<'PY'
import os
from pathlib import Path
import subprocess
import sys

root = Path(sys.argv[1])
hook = Path(subprocess.check_output(
    ["git", "rev-parse", "--path-format=absolute", "--git-path", "hooks/pre-push"],
    cwd=root, text=True,
).strip())
body = '''#!/usr/bin/env bash
# ee rustfmt pre-push gate (bd-gq26a); whole working tree, read-only.
set -eu
repo_root="$(git rev-parse --show-toplevel)"
exec "$repo_root/scripts/check-format.sh"
'''
if hook.exists() and hook.read_text() != body:
    if "# mcp-agent-mail chain-runner (pre-push)" not in hook.read_text():
        raise SystemExit(f"Refusing to replace unknown pre-push hook: {hook}")
    if not os.access(hook, os.X_OK):
        raise SystemExit(f"Existing pre-push dispatcher is not executable: {hook}")
    hook = hook.parent / "hooks.d" / "pre-push" / "40-rustfmt.sh"
if hook.is_symlink():
    raise SystemExit(f"Refusing to replace hook symlink: {hook}")
if hook.exists():
    if hook.read_text() != body or not os.access(hook, os.X_OK):
        raise SystemExit(f"Existing hook differs or is not executable: {hook}")
    print(f"[check-format] already installed in this clone: {hook}")
else:
    hook.parent.mkdir(parents=True, exist_ok=True)
    # Exclusive creation also protects against another installer winning a race.
    with hook.open("x") as destination:
        destination.write(body)
    hook.chmod(0o755)
    print(f"[check-format] installed in this clone only: {hook}")
print("[check-format] other clones require their own explicit installation")
PY
}

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
    # `cargo +toolchain` fails where cargo is not the rustup shim. Availability
    # was checked by resolve_toolchain; another nightly is never a substitute.
    # No --config-path: leave per-file configuration discovery to rustfmt.
    fmt_cmd=(rustup run "$PINNED_TOOLCHAIN" cargo fmt --check)

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
    if ! command -v rustup >/dev/null 2>&1; then
        warn "rustfmt not found on PATH — cannot check formatting, not blocking"
        return 2
    fi
    timeout_bin="$(resolve_timeout)"
    if [ -n "$timeout_bin" ]; then
        out="$("$timeout_bin" "$TIMEOUT_SECS" rustup run "$PINNED_TOOLCHAIN" rustfmt --edition "$EDITION" --check "$@" 2>&1)"
        rc=$?
    else
        warn "no timeout binary (timeout/gtimeout) — running without a hard cap"
        out="$(rustup run "$PINNED_TOOLCHAIN" rustfmt --edition "$EDITION" --check "$@" 2>&1)"
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
# Standalone and Cargo fixtures live outside the repo tree.
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

    # Exercise the production cargo invocation, not just standalone rustfmt.
    # The nested config deliberately disagrees with the root config. Passing
    # --config-path at the root would reject this correctly tab-indented file.
    mkdir -p "$tmp/crate/src"
    printf '[package]\nname = "format-control"\nversion = "0.0.0"\nedition = "2024"\n' > "$tmp/crate/Cargo.toml"
    printf 'edition = "2024"\nhard_tabs = false\n' > "$tmp/crate/rustfmt.toml"
    printf 'edition = "2024"\nhard_tabs = true\n' > "$tmp/crate/src/rustfmt.toml"
    printf 'fn main() {\n\tprintln!("control");\n}\n' > "$tmp/crate/src/main.rs"
    local rc4 rc5 rc6
    ( cd "$tmp/crate" && run_cargo_fmt_check ) > "$tmp/cargo-clean.log" 2>&1
    rc4=$?
    if [ "$rc4" -eq 0 ]; then
        note "self-test arm4 OK: cargo honors the source directory's config"
    else
        warn "SELF-TEST FAIL arm4: nested-config clean source gave rc=$rc4, expected 0"
        failures=$((failures + 1))
    fi
    printf 'fn main( ) {println!("control");}\n' > "$tmp/crate/src/main.rs"
    ( cd "$tmp/crate" && run_cargo_fmt_check ) > "$tmp/cargo-drift.log" 2>&1
    rc5=$?
    if [ "$rc5" -eq 1 ] && grep -q 'main.rs' "$tmp/cargo-drift.log"; then
        note "self-test arm5 OK: production cargo path catches planted drift"
    else
        warn "SELF-TEST FAIL arm5: planted drift gave rc=$rc5, expected 1 naming main.rs"
        failures=$((failures + 1))
    fi
    printf 'fn main() {\n\tprintln!("control");\n}\n' > "$tmp/crate/src/main.rs"
    ( cd "$tmp/crate" && run_cargo_fmt_check ) > "$tmp/cargo-restored.log" 2>&1
    rc6=$?
    if [ "$rc6" -eq 0 ]; then
        note "self-test arm6 OK: restored cargo fixture passes again"
    else
        warn "SELF-TEST FAIL arm6: restored source gave rc=$rc6, expected 0"
        failures=$((failures + 1))
    fi

    if [ "$failures" -ne 0 ]; then
        warn "SELF-TEST FAILED: ${failures} of 6 arms (logs retained in $tmp)"
        return 1
    fi
    note "self-test: 6 of 6 arms passed (fixtures retained in $tmp)"
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
    local script="$REPO_ROOT/scripts/lib/mod_reachability.py"
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
    cd "$REPO_ROOT" || exit 1
    if [ "${1:-}" = "--install-pre-push" ]; then
        install_pre_push
        exit $?
    fi
    local toolchain_available=1
    resolve_toolchain || toolchain_available=0
    case "${1:-}" in
        --self-test)
            [ "$toolchain_available" -eq 1 ] || exit 1
            self_test; exit $?
            ;;
        --reachability) run_reachability; exit $? ;;
        ""|--staged) ;;
        *) warn "usage: $0 [--staged|--self-test|--reachability|--install-pre-push]"; exit 1 ;;
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

    local rc=2
    if [ "$toolchain_available" -eq 1 ]; then
        run_cargo_fmt_check
        rc=$?
    fi
    local failed=0
    case "$rc" in
        0) note "formatting clean (cargo fmt --check, toolchain ${PINNED_TOOLCHAIN:-default})" ;;
        1)
            warn "FORMATTING DRIFT in the files listed above."
            warn "Repair the printed diff, then rerun: rustup run $PINNED_TOOLCHAIN cargo fmt --check"
            warn "Per-file repair tool: rustup run $PINNED_TOOLCHAIN rustfmt --edition 2024 <file> (this script never writes)"
            failed=1
            ;;
        *) : ;;   # inconclusive; run_cargo_fmt_check already warned
    esac

    # Runs even when formatting failed: reporting one finding and stopping is
    # how a check reports one defect and never five.
    if ! run_reachability; then
        failed=1
    fi

    # CI Static also runs this separate gate. Cargo's module walker does not
    # cover include!-only files; reuse CI's population resolver and invocation.
    if [ "$toolchain_available" -eq 1 ]; then
        if ! "$REPO_ROOT/scripts/check-include-fmt.sh"; then
            failed=1
        fi
    else
        warn "include-only formatting unavailable — inconclusive, not blocking"
    fi

    exit "$failed"
}

main "$@"
