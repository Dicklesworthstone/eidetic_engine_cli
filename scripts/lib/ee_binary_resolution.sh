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
        cargo metadata --no-deps --format-version 1 --manifest-path "$REPO_ROOT/Cargo.toml" 2>/dev/null |
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
