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

ee_resolve_binary() {
    local profile="${1:-debug}"
    local target_dir

    if [ -n "${EE_BINARY:-}" ]; then
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
