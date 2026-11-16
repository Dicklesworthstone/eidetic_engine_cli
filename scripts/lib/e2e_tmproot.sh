#!/usr/bin/env bash
# bd-mfqa2 — THE resolver for the e2e temp root. One implementation.
#
# WHY IT IS ITS OWN FILE. Sixteen e2e scripts each re-implemented this default,
# and the reason the twelve standalone ones could not simply use the shared
# version is that it lived inside scripts/lib/e2e_harness.sh — sourcing which
# also brings logging, assertion counters, EE_BIN resolution and traps that a
# standalone script never asked for. So the choice looked like "duplicate the
# logic" or "adopt a whole harness", and twelve scripts picked duplication.
#
# This file is the third option and it is deliberately TINY: no state, no traps,
# no output, one function. e2e_harness.sh sources it too, so there is exactly
# one implementation with two entry points rather than a rival helper beside the
# one it was written to replace.
#
# THE RULE IT ENCODES. /private/tmp is a macOS convention. It also EXISTS on the
# Linux fleet — root-owned and unwritable — so `[ -d /private/tmp ]` is true
# precisely where the path cannot be used, and scripts that gated on existence
# died at their first mktemp having executed ZERO assertions (bd-13y74). The
# ExFAT-avoidance reason for preferring /private/tmp on macOS is real
# (bd-2vq2z); it is a PLATFORM preference, not a universal default.
#
# NON-EMPTY IS LOAD-BEARING. An empty root is what let a cleanup guard's
# `"${EE_E2E_TMPDIR%/}"/*` collapse to the pattern `/*`, which matches every
# absolute path (fixed at 081e766fc). Callers may interpolate this value into
# patterns, so it must never return empty.

if [ -z "${_EE_TMPROOT_SOURCED:-}" ]; then
_EE_TMPROOT_SOURCED=1

# e2e_temp_root — print the temp root this host should use.
#   1. an explicit EE_E2E_TMPDIR always wins (caller's choice is never overridden)
#   2. macOS prefers /private/tmp when it exists
#   3. everything else takes TMPDIR, then /tmp
# Always prints a non-empty path with no trailing slash.
e2e_temp_root() {
    local root="${EE_E2E_TMPDIR:-}"
    if [ -z "$root" ]; then
        if [ "$(uname -s 2>/dev/null || printf 'unknown')" = "Darwin" ] && [ -d /private/tmp ]; then
            root="/private/tmp"
        else
            root="${TMPDIR:-/tmp}"
        fi
    fi
    root="${root%/}"
    [ -n "$root" ] || root="/tmp"
    printf '%s' "$root"
}

# ee_isolate_data_dirs <root_dir> — Redirect HOME and XDG_{DATA,CONFIG,CACHE,STATE}_HOME
# to <root_dir> and default EE_EMBED_DOWNLOAD=off so shell e2e scripts never
# read or mutate the runner host's ~/.local/share/ee store or model cache (bd-xuznt).
ee_isolate_data_dirs() {
    local root="${1:?ee_isolate_data_dirs: root directory required}"
    mkdir -p "$root/home" "$root/xdg/data" "$root/xdg/config" "$root/xdg/cache" "$root/xdg/state"
    if [ -z "${EE_EMBED_MODEL_DIR:-}" ] && [ -n "${EE_EMBED_MODEL_FIXTURE_DIR:-}" ]; then
        export EE_EMBED_MODEL_DIR="$EE_EMBED_MODEL_FIXTURE_DIR"
    fi
    export HOME="$root/home"
    export XDG_DATA_HOME="$root/xdg/data"
    export XDG_CONFIG_HOME="$root/xdg/config"
    export XDG_CACHE_HOME="$root/xdg/cache"
    export XDG_STATE_HOME="$root/xdg/state"
    export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
}

fi
