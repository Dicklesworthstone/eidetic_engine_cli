#!/usr/bin/env bash
# Format gate for files reached ONLY via `include!` (bd-39y21).
#
# WHY THIS EXISTS. `cargo fmt --check` walks the module tree via `mod` and
# `#[path]`. It never follows `include!`. A file that is textually inlined by an
# `include!` therefore COMPILES but is invisible to the Format step, so a green
# `cargo fmt --check` says nothing about it.
#
# Measured 2026-09-21: src/cass/backfill_public_tests.rs carried 329 lines of
# rustfmt diff while `cargo fmt --check` exited 0. Not a bug in rustfmt and not a
# bug in the Format step -- the step's POPULATION is module-reachable files, and
# that file is outside it. See docs/testing-strategy.md, "Every Gate Has A
# Population, And Green Only Covers That Population".
#
# THE LIST IS NOT RE-DERIVED HERE. `scripts/lib/mod_reachability.py` already
# computes the include!-only set, and this script consumes it via
# `--list-include-only`. A second walker with its own regex would be a second
# copy of the rule, and the two would disagree the first time an `include!` form
# changed.
#
# Exit codes are distinct because "nothing to check" and "everything passed" are
# different answers:
#   0  every include!-only file is formatted (or there are none, stated so)
#   1  at least one is not formatted -- names each
#   2  the file list could not be determined (cargo unavailable); NOT a pass
#   3  the pinned toolchain or rustfmt is unavailable; NOT a pass
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "$REPO_ROOT"

# The toolchain is read from rust-toolchain.toml rather than assumed, because
# rustfmt output differs between toolchains and a check run under a different one
# would report a diff nobody can reproduce (reference: half a pin reddens a green
# workflow).
TOOLCHAIN="$(grep -oE 'nightly-[0-9-]+' rust-toolchain.toml | head -1 || true)"
if [ -z "$TOOLCHAIN" ]; then
    printf 'check-include-fmt: could not read the pinned toolchain from rust-toolchain.toml\n' >&2
    printf 'check-include-fmt: refusing to format-check under an unknown toolchain.\n' >&2
    exit 3
fi
if ! rustup run "$TOOLCHAIN" rustfmt --version >/dev/null 2>&1; then
    printf 'check-include-fmt: rustfmt unavailable under %s.\n' "$TOOLCHAIN" >&2
    printf 'check-include-fmt: this is an ENVIRONMENT error, not a formatting verdict.\n' >&2
    exit 3
fi

files_raw=""
if ! files_raw="$(python3 scripts/lib/mod_reachability.py --list-include-only 2>/dev/null)"; then
    printf 'check-include-fmt: could not determine the include!-only file set.\n' >&2
    printf 'check-include-fmt: an unknown population is NOT a pass -- it is the same\n' >&2
    printf 'check-include-fmt: unknown a skipped check would be.\n' >&2
    exit 2
fi

checked=0
unformatted=()
while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    [ -f "$rel" ] || continue
    checked=$((checked + 1))
    if ! rustup run "$TOOLCHAIN" rustfmt --check --edition 2024 "$rel" >/dev/null 2>&1; then
        unformatted+=("$rel")
    fi
done <<<"$files_raw"

# PRINT THE POPULATION, ALWAYS. A gate that says only "ok" leaves a reader unable
# to tell a real pass from a run that checked nothing -- which is the exact
# failure this gate was created to close.
if [ "$checked" -eq 0 ]; then
    printf 'check-include-fmt: no include!-only files found; nothing to check.\n' >&2
    exit 0
fi

if [ "${#unformatted[@]}" -ne 0 ]; then
    printf 'check-include-fmt: %s of %s include!-only file(s) are NOT formatted:\n' \
        "${#unformatted[@]}" "$checked" >&2
    printf '  %s\n' "${unformatted[@]}" >&2
    printf 'check-include-fmt: `cargo fmt --check` cannot see these -- rustfmt follows\n' >&2
    printf 'check-include-fmt: mod/#[path], never include! -- so a green Format step\n' >&2
    printf 'check-include-fmt: does not cover them. Run:\n' >&2
    printf '  rustup run %s rustfmt --edition 2024 %s\n' "$TOOLCHAIN" "${unformatted[*]}" >&2
    exit 1
fi

printf 'check-include-fmt: %s include!-only file(s) formatted (toolchain %s)\n' \
    "$checked" "$TOOLCHAIN" >&2
exit 0
