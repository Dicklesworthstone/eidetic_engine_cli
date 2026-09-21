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

fi
