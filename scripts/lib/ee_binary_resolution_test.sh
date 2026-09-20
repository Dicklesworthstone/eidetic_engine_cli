#!/usr/bin/env bash
# Unit checks for scripts/lib/ee_binary_resolution.sh.
#
# This test is intentionally shell-only: it stubs cargo and jq so it can prove
# target-directory resolution without starting a Cargo build.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/lib/ee_binary_resolution.sh
source "$REPO_ROOT/scripts/lib/ee_binary_resolution.sh"

assert_eq() {
    local actual="${1:?actual required}"
    local expected="${2:?expected required}"
    local label="${3:?label required}"

    if [ "$actual" != "$expected" ]; then
        printf 'FAIL %s\nexpected: %s\nactual:   %s\n' "$label" "$expected" "$actual" >&2
        exit 1
    fi
    printf 'ok %s\n' "$label"
}

# assert_eq cannot express "produced nothing": its `${1:?actual required}` aborts
# on an empty first argument, so an emptiness check written with it fails for the
# wrong reason. The silent cases matter here -- a warning that also fires when
# nothing is wrong is noise, and noise is how a warning gets ignored -- so they
# get an assertion that can actually represent empty.
assert_silent() {
    local actual="${1-}"
    local label="${2:?label required}"

    if [ -n "$actual" ]; then
        printf 'FAIL %s\nexpected: (no output)\nactual:   %s\n' "$label" "$actual" >&2
        exit 1
    fi
    printf 'ok %s\n' "$label"
}

SCRATCH_ROOT="${TMPDIR:-/tmp}/ee-binary-resolution-test.$$"
FAKE_BIN="$SCRATCH_ROOT/bin"
mkdir -p "$FAKE_BIN"

cat >"$FAKE_BIN/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "$#" -ge 1 ] && [ "$1" = "metadata" ]; then
    printf '{"target_directory":"/fixture/cargo-metadata-target"}\n'
    exit 0
fi
printf 'unexpected cargo invocation: %s\n' "$*" >&2
exit 2
SH

cat >"$FAKE_BIN/jq" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
cat >/dev/null
printf '/fixture/cargo-metadata-target\n'
SH

chmod +x "$FAKE_BIN/cargo" "$FAKE_BIN/jq"

explicit_binary="$(
    EE_BINARY="/custom/bin/ee" \
        CARGO_TARGET_DIR="/external/target" \
        PATH="$FAKE_BIN:$PATH" \
        ee_resolve_binary release
)"
assert_eq "$explicit_binary" "/custom/bin/ee" "explicit EE_BINARY wins"

cargo_target_binary="$(
    unset EE_BINARY
    CARGO_TARGET_DIR="/Volumes/USBNVME16TB/temp_agent_space/cargo-target" \
        PATH="$FAKE_BIN:$PATH" \
        ee_resolve_binary debug
)"
assert_eq \
    "$cargo_target_binary" \
    "/Volumes/USBNVME16TB/temp_agent_space/cargo-target/debug/ee" \
    "CARGO_TARGET_DIR determines debug binary"

metadata_binary="$(
    unset EE_BINARY CARGO_TARGET_DIR
    PATH="$FAKE_BIN:$PATH" ee_resolve_binary release
)"
assert_eq \
    "$metadata_binary" \
    "/fixture/cargo-metadata-target/release/ee" \
    "cargo metadata target_directory fallback"

fallback_binary="$(
    unset EE_BINARY CARGO_TARGET_DIR
    PATH="/nonexistent" ee_resolve_binary debug
)"
assert_eq "$fallback_binary" "$REPO_ROOT/target/debug/ee" "repo target fallback"

printf 'scratch retained for audit: %s\n' "$SCRATCH_ROOT"

# ---------------------------------------------------------------------------
# bd-smxdr: staleness guard. Resolution alone never caught the real defect --
# a stage can resolve a real, executable binary that is two minor versions
# behind and still report PASS. These arms pin the refusal AND the acceptance:
# an over-broad guard that refuses everything would also "fix" the symptom
# while breaking every stage, so the current-binary arm is load-bearing.
# ---------------------------------------------------------------------------

GUARD_BIN_DIR="$SCRATCH_ROOT/guard"
mkdir -p "$GUARD_BIN_DIR"

guard_source_version="$(ee_source_version)"
if [ -z "$guard_source_version" ]; then
    printf 'FAIL ee_source_version returned empty; Cargo.toml parse broke\n' >&2
    exit 1
fi
printf 'ok ee_source_version reads Cargo.toml (%s)\n' "$guard_source_version"

# A binary reporting exactly the source version is accepted.
cat >"$GUARD_BIN_DIR/ee-current" <<SH
#!/bin/sh
echo "ee $guard_source_version"
SH

# A binary two minor versions behind is the measured real-world case.
cat >"$GUARD_BIN_DIR/ee-stale" <<'SH'
#!/bin/sh
echo "ee 0.14.2"
SH

# A binary that prints nothing usable must not be treated as current.
cat >"$GUARD_BIN_DIR/ee-mute" <<'SH'
#!/bin/sh
exit 0
SH

chmod +x "$GUARD_BIN_DIR/ee-current" "$GUARD_BIN_DIR/ee-stale" "$GUARD_BIN_DIR/ee-mute"

guard_verdict() {
    if ee_require_current_binary "$1" "guard-test" 2>/dev/null; then
        printf 'accepted\n'
    else
        printf 'refused\n'
    fi
}

assert_eq "$(guard_verdict "$GUARD_BIN_DIR/ee-current")" "accepted" \
    "current-version binary is accepted (guard is not over-broad)"
assert_eq "$(guard_verdict "$GUARD_BIN_DIR/ee-stale")" "refused" \
    "stale binary is refused (bd-smxdr countermetric)"
assert_eq "$(guard_verdict "$GUARD_BIN_DIR/ee-mute")" "refused" \
    "binary with unreadable version is refused, not assumed current"
assert_eq "$(guard_verdict "$GUARD_BIN_DIR/does-not-exist")" "refused" \
    "missing binary is refused"

# Provenance must be printed even on the PASSING path, so a log reader can see
# which binary produced a verdict. Asserting emptiness would prove nothing.
guard_pass_log="$(ee_require_current_binary "$GUARD_BIN_DIR/ee-current" "guard-test" 2>&1 >/dev/null || true)"
case "$guard_pass_log" in
    *"ee_binary=$GUARD_BIN_DIR/ee-current"*"binary_version=$guard_source_version"*"source_version=$guard_source_version"*)
        printf 'ok passing path still records binary path and both versions\n'
        ;;
    *)
        printf 'FAIL passing path did not record provenance\nactual: %s\n' "$guard_pass_log" >&2
        exit 1
        ;;
esac

# ---------------------------------------------------------------------------
# bd-udjrq: wrong-PLATFORM guard. The staleness arms above all assume the
# binary runs. Measured on the Mac dev host 2026-09-18, it did not: the shared
# Cargo target directory held Linux x86-64 ELF `debug/ee` and `release/ee`
# (the RCH-E327 wrong-platform-artifact class), `[ -x ]` accepted both because
# the executable BIT was set, and scripts/e2e_session_budget.sh reported 15
# assert_fails whose single cause was `exec format error` behind exit 126.
#
# Both arms are load-bearing. A guard that refuses every binary would also
# silence those 15 failures, so the acceptance arm is what stops the refusal
# from becoming a blanket one -- and the stale-binary arm below pins the
# distinction the guard must NOT collapse: a stale binary RUNS, and is
# rejected later, by version, with a different message.
# ---------------------------------------------------------------------------

PLATFORM_BIN_DIR="$SCRATCH_ROOT/platform"
mkdir -p "$PLATFORM_BIN_DIR"

# A file with the executable bit set and a header no kernel will load. Built
# from garbage rather than copied from the real ELF so the fixture is invalid
# on Linux CI too, where a genuine x86-64 ELF would simply run.
printf '\177ELF\002\001\001\000\000\000\000\000\000\000\000\000not-a-real-binary' \
    >"$PLATFORM_BIN_DIR/ee-foreign"
chmod +x "$PLATFORM_BIN_DIR/ee-foreign"

if [ ! -x "$PLATFORM_BIN_DIR/ee-foreign" ]; then
    printf 'FAIL fixture setup: ee-foreign is not marked executable, so the\n' >&2
    printf '     arm below would pass for the wrong reason (-x, not format).\n' >&2
    exit 1
fi
printf 'ok fixture ee-foreign has the executable bit set (refusal must come from format)\n'

platform_verdict() {
    if ee_binary_executes_here "$1"; then printf 'executes\n'; else printf 'refused\n'; fi
}

assert_eq "$(platform_verdict "$PLATFORM_BIN_DIR/ee-foreign")" "refused" \
    "binary with the executable bit but an unloadable format is refused"
assert_eq "$(platform_verdict "$GUARD_BIN_DIR/ee-current")" "executes" \
    "current binary still executes (platform guard is not over-broad)"
assert_eq "$(platform_verdict "$GUARD_BIN_DIR/ee-stale")" "executes" \
    "a STALE binary executes -- the platform guard must not absorb the version check"
assert_eq "$(platform_verdict "$GUARD_BIN_DIR/does-not-exist")" "refused" \
    "missing binary is refused"

# The refusal has to name the real cause. "could not determine both binary and
# source versions" was the message this case produced before the guard existed,
# and it sends a reader looking for a version bug instead of a foreign binary.
platform_log="$(ee_require_current_binary "$PLATFORM_BIN_DIR/ee-foreign" "platform-test" 2>&1 >/dev/null || true)"
case "$platform_log" in
    *"format foreign to"*"cannot execute on this host"*)
        printf 'ok refusal names the format, not a missing version\n'
        ;;
    *)
        printf 'FAIL wrong-platform refusal did not name the format cause\nactual: %s\n' \
            "$platform_log" >&2
        exit 1
        ;;
esac

# Counterpart: the STALE refusal must still read as a version problem, or the
# two diagnoses have been merged into one unhelpful message.
stale_log="$(ee_require_current_binary "$GUARD_BIN_DIR/ee-stale" "platform-test" 2>&1 >/dev/null || true)"
case "$stale_log" in
    *"refusing STALE binary"*) printf 'ok stale refusal still reads as a version problem\n' ;;
    *)
        printf 'FAIL stale refusal lost its version wording\nactual: %s\n' "$stale_log" >&2
        exit 1
        ;;
esac

# ---------------------------------------------------------------------------
# bd-3vuhv: a preset EE_BINARY whose profile contradicts the requested one must
# ANNOUNCE itself. The silence was the defect -- shared.sh:33 asked for release,
# received a debug build, and could not tell. Measured on an RCH worker,
# `ee init`: release 6.18s mean (n=5) vs debug 62.3s mean (n=2), so a 60s budget
# calibrated for release fires against debug and reports a timeout, not a
# profile.
# ---------------------------------------------------------------------------

# 1. THE PATH MUST STILL BE CLEAN. The warning goes to stderr; if it ever
#    reached stdout every caller's `$(...)` would capture it as part of the path,
#    turning an advisory into a broken harness.
mismatch_path="$(
    EE_BINARY="/repo/.rch-target/debug/ee" \
        PATH="$FAKE_BIN:$PATH" \
        ee_resolve_binary release 2>/dev/null
)"
assert_eq "$mismatch_path" "/repo/.rch-target/debug/ee" \
    "mismatch still returns EE_BINARY verbatim on clean stdout"

# 2. AND IT MUST ACTUALLY WARN. A silent pass here is the original defect.
mismatch_log="$(
    EE_BINARY="/repo/.rch-target/debug/ee" \
        PATH="$FAKE_BIN:$PATH" \
        ee_resolve_binary release 2>&1 >/dev/null
)"
case "$mismatch_log" in
    *"PROFILE MISMATCH"*"requested release"*"debug build"*)
        printf 'ok profile mismatch is announced with both profiles named\n'
        ;;
    *)
        printf 'FAIL profile mismatch was not announced\nactual: %s\n' "$mismatch_log" >&2
        exit 1
        ;;
esac

# 3. THE OPPOSITE DIRECTION TOO, so the check is not keyed to one profile.
reverse_log="$(
    EE_BINARY="/repo/target/release/ee" \
        PATH="$FAKE_BIN:$PATH" \
        ee_resolve_binary debug 2>&1 >/dev/null
)"
case "$reverse_log" in
    *"PROFILE MISMATCH"*"requested debug"*"release build"*)
        printf 'ok reverse mismatch (release binary, debug requested) is announced\n'
        ;;
    *)
        printf 'FAIL reverse mismatch was not announced\nactual: %s\n' "$reverse_log" >&2
        exit 1
        ;;
esac

# 4. NEGATIVE CONTROL -- AGREEMENT MUST BE SILENT. A warning that fires on the
#    correct case is noise, and noise is how a warning gets ignored.
agree_log="$(
    EE_BINARY="/repo/target/release/ee" \
        PATH="$FAKE_BIN:$PATH" \
        ee_resolve_binary release 2>&1 >/dev/null
)"
assert_silent "$agree_log" "matching profile produces no warning"

# 5. NEGATIVE CONTROL -- AN UNKNOWN PROFILE IS NOT A MISMATCH. A custom path
#    carries no Cargo profile segment; guessing one would warn on every such
#    caller and train readers to ignore the message.
custom_log="$(
    EE_BINARY="/custom/bin/ee" \
        PATH="$FAKE_BIN:$PATH" \
        ee_resolve_binary release 2>&1 >/dev/null
)"
assert_silent "$custom_log" "path with no profile segment produces no warning"

# 6. NEGATIVE CONTROL -- NO PROFILE REQUESTED, NO PROMISE TO BREAK. `${1:-debug}`
#    cannot tell "asked for debug" from "asked for nothing"; only the first is a
#    claim worth checking, so a bare call must stay quiet.
implicit_log="$(
    EE_BINARY="/repo/target/release/ee" \
        PATH="$FAKE_BIN:$PATH" \
        ee_resolve_binary 2>&1 >/dev/null
)"
assert_silent "$implicit_log" "no explicit profile requested produces no warning"
