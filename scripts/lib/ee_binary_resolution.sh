#!/usr/bin/env bash
# Resolve the ee binary from Cargo's effective target directory.
#
# Source this file after REPO_ROOT is set. The helper avoids assuming the
# repository-local target directory when CARGO_TARGET_DIR or Cargo metadata
# points somewhere else.

ee_cargo_target_directory() {
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        printf '%s\n' "${CARGO_TARGET_DIR%/}"
        return 0
    fi

    if command -v cargo >/dev/null 2>&1 && command -v jq >/dev/null 2>&1; then
        cargo metadata --locked --no-deps --format-version 1 --manifest-path "$REPO_ROOT/Cargo.toml" 2>/dev/null |
            jq -r '.target_directory // empty' 2>/dev/null |
            sed -n '1p'
    fi
}

# Profile implied by a binary path's Cargo layout segment. Prints `debug`,
# `release`, or NOTHING when the path carries neither -- an unknown profile is
# not a mismatch, and guessing one would manufacture warnings for every custom
# path someone passes.
ee_binary_path_profile() {
    case "${1:-}" in
    */debug/ee | */debug/ee.exe) printf 'debug\n' ;;
    */release/ee | */release/ee.exe) printf 'release\n' ;;
    *) : ;;
    esac
}

ee_resolve_binary() {
    # Whether the caller NAMED a profile. `${1:-debug}` cannot distinguish "asked
    # for debug" from "asked for nothing", and only the first is a promise worth
    # checking against.
    local requested_explicitly=0
    if [ "$#" -ge 1 ] && [ -n "${1:-}" ]; then
        requested_explicitly=1
    fi
    local profile="${1:-debug}"
    local target_dir actual_profile

    if [ -n "${EE_BINARY:-}" ]; then
        # bd-3vuhv. A PRESET EE_BINARY WINS OVER THE REQUESTED PROFILE, AND USED
        # TO DO SO SILENTLY. That silence is the defect this announces.
        #
        # scripts/e2e_overhaul/lib/shared.sh:33 asks for `release` and had no way
        # to learn it got a debug build instead. Measured on an RCH worker,
        # `ee init` into a fresh workspace: RELEASE 6.18s mean (n=5), DEBUG 62.3s
        # mean (n=2, plus n=10 more debug runs at 61.6-62.7s from a separate
        # arm). Ten times slower. The library's 60s init budget has ~54s of
        # headroom for release and misses by ~2s for debug, so every suite
        # sourcing it died at status=124 -- reported as "ee init failed", which
        # reads as a broken init rather than a wrong build.
        #
        # DELIBERATELY NOT FATAL AND DELIBERATELY NOT A RESOLUTION CHANGE.
        # Pinning EE_BINARY to a debug build is legitimate; several suites ask
        # for `debug` on purpose. Changing which binary is returned would
        # re-point every caller at once, unmeasured, in an area with no hosted
        # gate. The harness only needs to be ABLE TO TELL. Downstream can then
        # decide to refuse, the way ee_require_current_binary refuses staleness.
        #
        # stderr, never stdout: callers use `$(ee_resolve_binary ...)` and a
        # warning on stdout would be captured as part of the path.
        if [ "$requested_explicitly" -eq 1 ]; then
            actual_profile="$(ee_binary_path_profile "$EE_BINARY")"
            if [ -n "$actual_profile" ] && [ "$actual_profile" != "$profile" ]; then
                printf 'ee_resolve_binary: PROFILE MISMATCH: caller requested %s, EE_BINARY is a %s build\n' \
                    "$profile" "$actual_profile" >&2
                printf 'ee_resolve_binary:   EE_BINARY=%s\n' "$EE_BINARY" >&2
                printf 'ee_resolve_binary: using it unchanged. A %s build ran `ee init` ~10x slower than\n' \
                    "$actual_profile" >&2
                printf 'ee_resolve_binary: %s on an RCH worker (62.3s vs 6.18s), so timeouts calibrated for\n' \
                    "$profile" >&2
                printf 'ee_resolve_binary: %s will fire against it and report a timeout, not a profile (bd-3vuhv).\n' \
                    "$profile" >&2
            fi
        fi
        printf '%s\n' "$EE_BINARY"
        return 0
    fi

    target_dir="$(ee_cargo_target_directory || true)"
    if [ -n "$target_dir" ]; then
        printf '%s/%s/ee\n' "${target_dir%/}" "$profile"
    else
        printf '%s/target/%s/ee\n' "$REPO_ROOT" "$profile"
    fi
}

# ---------------------------------------------------------------------------
# Staleness guard (bd-smxdr).
#
# Resolving a path is not enough: a stage can resolve a real, executable binary
# that is months behind the source and still report PASS. Measured on the Mac
# dev host 2026-09-16, `ee` on PATH was 0.14.2 while Cargo.toml was 0.15.2, so
# any stage inheriting PATH was asserting current behaviour against a binary
# that could not contain it.
#
# `ee_require_current_binary` refuses that case. It is deliberately fatal
# rather than a warning: a gate that proves nothing is worse than a gate that
# fails, because it carries the authority of a verification stage.
# ---------------------------------------------------------------------------

# Package version from Cargo.toml -- the authority for "current". Only the
# `[package]` version starts at column 0; dependency versions live inside
# inline tables, so the anchored match cannot pick one up by accident.
ee_source_version() {
    sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
        "$REPO_ROOT/Cargo.toml" | sed -n '1p'
}

# Refuse a binary that cannot execute on THIS host.
#
# `[ -x ]` tests the executable BIT, not the executable FORMAT. Measured on
# the Mac dev host 2026-09-18: the shared Cargo target directory held
# `debug/ee` and `release/ee` that were Linux x86-64 ELF, written by an RCH
# run (the RCH-E327 wrong-platform-artifact class). Both satisfied `-x`.
# scripts/e2e_session_budget.sh ran against one and reported 15 assert_fails
# whose single real cause was `exec format error` behind a 126 exit -- which
# reads as fifteen product defects rather than one environment fault.
ee_binary_executes_here() {
    local binary="${1:?ee_binary_executes_here: binary path required}"
    local status=0
    "$binary" --version >/dev/null 2>&1 || status=$?
    # 126 = found but not executable here (foreign architecture or format,
    # missing interpreter); 127 = not found. Every other status means the
    # binary RAN, and a non-zero exit from an unsupported flag is not this
    # helper's concern -- widening the case list would turn a product failure
    # into an environment excuse.
    case "$status" in
        126 | 127) return 1 ;;
        *) return 0 ;;
    esac
}

# Version reported by a built binary. `ee --version` prints `ee <semver>`.
ee_binary_version() {
    local binary="${1:?ee_binary_version: binary path required}"
    "$binary" --version 2>/dev/null | awk 'NR==1 {print $NF}'
}

# Echo provenance, then refuse anything that is missing or not the current
# source version. Always prints the resolved path and both versions BEFORE
# deciding, so a log reader can see which binary produced a verdict even when
# the check passes.
ee_require_current_binary() {
    local binary="${1:?ee_require_current_binary: binary path required}"
    local label="${2:-ee}"
    local source_version binary_version

    if [ ! -x "$binary" ]; then
        printf '%s: ee_binary=%s (missing or not executable)\n' "$label" "$binary" >&2
        printf '%s: refusing to run against a binary that does not exist.\n' "$label" >&2
        return 1
    fi

    if ! ee_binary_executes_here "$binary"; then
        printf '%s: ee_binary=%s (executable bit set, format foreign to %s/%s)\n' \
            "$label" "$binary" "$(uname -s)" "$(uname -m)" >&2
        printf '%s: file(1): %s\n' \
            "$label" "$(file -b "$binary" 2>/dev/null || printf 'unavailable')" >&2
        printf '%s: refusing: this binary cannot execute on this host, so every\n' "$label" >&2
        printf '%s: assertion run against it would fail for the same one reason.\n' "$label" >&2
        return 1
    fi

    source_version="$(ee_source_version)"
    binary_version="$(ee_binary_version "$binary")"
    printf '%s: ee_binary=%s binary_version=%s source_version=%s\n' \
        "$label" "$binary" "${binary_version:-unknown}" "${source_version:-unknown}" >&2

    if [ -z "$source_version" ] || [ -z "$binary_version" ]; then
        printf '%s: refusing: could not determine both binary and source versions.\n' \
            "$label" >&2
        return 1
    fi

    if [ "$binary_version" != "$source_version" ]; then
        printf '%s: refusing STALE binary: %s reports %s but Cargo.toml is %s.\n' \
            "$label" "$binary" "$binary_version" "$source_version" >&2
        printf '%s: build the current source and pass it via EE_BIN/EE_BINARY.\n' \
            "$label" >&2
        return 1
    fi

    return 0
}

# Hash the candidate binary. Prints the sha256 on stdout, or nothing and a
# non-zero status when neither hashing tool is available or the file is gone.
# Kept separate from the comparison so "cannot hash" and "hash differs" stay two
# answers rather than one.
ee_binary_sha256() {
    local binary="${1:?ee_binary_sha256: binary path required}"
    local sum

    [ -f "$binary" ] || return 2
    sum="$(
        { shasum -a 256 "$binary" 2>/dev/null || sha256sum "$binary" 2>/dev/null; } \
            | awk 'NR==1 {print $1}'
    )"
    [ -n "$sum" ] || return 2
    printf '%s\n' "$sum"
}

# Compare the candidate binary NOW against the hash recorded at the start of a
# run (bd-reality-core-convergence-1azkt.5, bullet 5, "hash").
#
# WHY A SECOND OBSERVATION IS THE ONLY HONEST SECOND VALUE. The bullet wants the
# binary's hash VERIFIED, not merely recorded. Nothing in the tree declares an
# expected ee hash and nothing can: the binary is built per run, so a static
# manifest cannot name its digest, and inventing one would be provenance
# fabrication. What CAN be compared is the same binary at two points in time.
#
# WHY THAT IS NOT A TAUTOLOGY. verify.sh hashes EE_BINARY once, before any
# stage, and a later stage runs `cargo build --locked --bin ee`
# (scripts/verify.sh:1923, inside "Write Contention E2E"). If that build
# replaces the file, stages before it and stages after it ran DIFFERENT
# binaries, and the identity printed at the top of the log -- the one a proof
# capsule binds -- does not describe what produced the later verdicts. That is
# the "wrong binary / stale source" case acceptance bullet 4 requires to make a
# run non-successful, and until this function existed nothing looked.
#
# Exit codes are distinct on purpose, because "unchanged", "changed" and "could
# not be hashed" are three states and one exit code cannot express two of them:
#   0  unchanged
#   1  changed -- prints both digests
#   2  cannot hash now (missing file or no hashing tool)
#   3  no baseline was recorded, so there is nothing to compare against
ee_assert_binary_identity_unchanged() {
    local binary="${1:?ee_assert_binary_identity_unchanged: binary path required}"
    local baseline="${2-}"
    local label="${3:-verify}"
    local current

    if [ -z "$baseline" ]; then
        printf '%s: candidate binary identity NOT CHECKED: no baseline digest was recorded.\n' \
            "$label" >&2
        return 3
    fi

    if ! current="$(ee_binary_sha256 "$binary")"; then
        printf '%s: candidate binary identity NOT CHECKED: %s could not be hashed now.\n' \
            "$label" "$binary" >&2
        return 2
    fi

    if [ "$current" = "$baseline" ]; then
        printf '%s: candidate binary identity stable: sha256=%s\n' "$label" "$current" >&2
        return 0
    fi

    printf '%s: CANDIDATE BINARY CHANGED DURING THE RUN.\n' "$label" >&2
    printf '%s:   at start: %s\n' "$label" "$baseline" >&2
    printf '%s:   now:      %s\n' "$label" "$current" >&2
    printf '%s: stages before and after the rebuild ran DIFFERENT binaries, so the\n' "$label" >&2
    printf '%s: identity recorded at the top of this log does not describe what\n' "$label" >&2
    printf '%s: produced the later verdicts.\n' "$label" >&2
    return 1
}
