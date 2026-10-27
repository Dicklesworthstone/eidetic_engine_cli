#!/usr/bin/env bash
# Unit checks for scripts/lib/appledouble_sidecar_probe.sh.
#
# The probe's whole job is to answer one question correctly in BOTH directions,
# so both directions are measured against real filesystems rather than
# simulated. The hazardous arm needs a mounted filesystem that shims extended
# attributes (ExFAT/FAT); when none is mounted it is announced as a SKIP rather
# than passing quietly, because a polarity arm nobody saw fail is decoration.
#
# Filesystem TYPE comes from `mount`; the verdict comes from the probe. The two
# sources are independent, so a probe that always answered the same way could
# not satisfy both polarity arms.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/lib/appledouble_sidecar_probe.sh
source "$REPO_ROOT/scripts/lib/appledouble_sidecar_probe.sh"

ARMS=0
POLARITY_ARMS_RUN=0

pass() {
    ARMS=$((ARMS + 1))
    printf 'ok %s\n' "$1"
}

fail() {
    printf 'FAIL %s\n' "$1" >&2
    shift
    [ "$#" -gt 0 ] && printf '%s\n' "$@" >&2
    exit 1
}

probe_rc() {
    local rc=0
    ee_dir_creates_appledouble_sidecars "$1" >/dev/null 2>&1 || rc=$?
    printf '%s' "$rc"
}

# A writable mount point whose filesystem type matches the given extended
# regex, or nothing. Reads `mount`, not the probe.
mount_point_of_type() {
    local type_re="$1" line point
    while IFS= read -r line; do
        case "$line" in
            *" on "*"("*) ;;
            *) continue ;;
        esac
        point="${line#* on }"
        point="${point% (*}"
        printf '%s' "${line##*(}" | grep -qE "^($type_re)," || continue
        [ -d "$point" ] && [ -w "$point" ] || continue
        printf '%s\n' "$point"
        return 0
    done < <(mount 2>/dev/null)
    return 1
}

# ---------------------------------------------------------------------------
# Indeterminate arms. These come first because they need no filesystem at all,
# so a host that can run nothing else still proves the probe fails SOFT rather
# than claiming a verdict it cannot support.
# ---------------------------------------------------------------------------

[ "$(probe_rc "")" = "2" ] ||
    fail "an empty directory argument must be indeterminate (2), not a verdict" \
        "got: $(probe_rc "")"
pass "empty argument is indeterminate"

missing_dir="${TMPDIR:-/tmp}/ee-appledouble-probe-absent.$$"
[ ! -e "$missing_dir" ] || fail "fixture setup: $missing_dir already exists"
[ "$(probe_rc "$missing_dir")" = "2" ] ||
    fail "a nonexistent directory must be indeterminate (2)" "got: $(probe_rc "$missing_dir")"
pass "nonexistent directory is indeterminate"

# A regular file is not a directory. Without this arm the `-d` check could be
# relaxed to `-e` and nothing else would notice.
file_not_dir="$(mktemp "${TMPDIR:-/tmp}/ee-appledouble-probe-file.XXXXXX")"
[ "$(probe_rc "$file_not_dir")" = "2" ] ||
    fail "a regular file must be indeterminate (2)" "got: $(probe_rc "$file_not_dir")"
pass "regular file is indeterminate"

# ---------------------------------------------------------------------------
# Polarity arm 1: a filesystem that stores xattrs natively must report 1.
# The repo's own checkout is used because it always exists; if the repo itself
# lived on a shimmed filesystem that is a real finding, not a skip.
# ---------------------------------------------------------------------------

native_point="$(mount_point_of_type 'apfs|hfs' || true)"
if [ -n "$native_point" ]; then
    native_rc="$(probe_rc "$REPO_ROOT")"
    case "$native_rc" in
        1)
            pass "xattr-native filesystem reports NO sidecars (repo root)"
            POLARITY_ARMS_RUN=$((POLARITY_ARMS_RUN + 1))
            ;;
        0) fail "the repo root's filesystem shims xattrs into AppleDouble sidecars" \
            "A committed-tree export here would reproduce RCH-E410." ;;
        *) fail "probe could not run on the repo root" "rc=$native_rc" ;;
    esac
else
    printf 'SKIP no apfs/hfs mount point found; the xattr-native polarity arm did not run\n'
fi

# ---------------------------------------------------------------------------
# Polarity arm 2: a filesystem that shims xattrs must report 0.
#
# This is the arm that matters, because it is the condition the guard refuses
# on. It also carries the PLANTED NEGATIVE below.
# ---------------------------------------------------------------------------

shim_point="$(mount_point_of_type 'exfat|msdos|vfat|fat' || true)"
if [ -n "$shim_point" ]; then
    shim_rc="$(probe_rc "$shim_point")"
    case "$shim_rc" in
        0)
            pass "xattr-shimming filesystem reports sidecars ($shim_point)"
            POLARITY_ARMS_RUN=$((POLARITY_ARMS_RUN + 1))
            ;;
        1) fail "a FAT-family filesystem reported no sidecars" \
            "Either the mount stores xattrs natively, or the probe has stopped detecting them: $shim_point" ;;
        *) fail "probe could not run on $shim_point" "rc=$shim_rc" ;;
    esac

    # THE PLANTED NEGATIVE. `xattr -w` SUCCEEDS on this filesystem -- that is
    # how the sidecar gets written. So a probe rewritten to key on the write's
    # exit status would report "fine" here, and every other arm in this file
    # would still pass. This arm fails the moment someone makes that
    # substitution, and it is the only arm that can.
    planted_dir="$(mktemp -d "$shim_point/.ee-xattr-planted.XXXXXX")"
    : >"$planted_dir/p"
    planted_write_rc=0
    /usr/bin/xattr -w com.apple.ee_rch_probe 1 "$planted_dir/p" >/dev/null 2>&1 ||
        planted_write_rc=$?
    [ "$planted_write_rc" = "0" ] ||
        fail "the xattr write was expected to SUCCEED on a shimming filesystem" \
            "If it now fails, the exit-status predicate this arm exists to forbid would \
work by accident, and the comment in the probe is stale. rc=$planted_write_rc"
    [ -e "$planted_dir/._p" ] ||
        fail "no sidecar appeared beside a successful xattr write on $shim_point"
    pass "the xattr write succeeds on a shimming filesystem, so its exit status cannot be the discriminator"
    unlink "$planted_dir/._p"
    unlink "$planted_dir/p"
    rmdir "$planted_dir"
else
    printf 'SKIP no exfat/msdos/vfat mount point found; the xattr-shimming polarity arm did not run\n'
fi

[ "$POLARITY_ARMS_RUN" -ge 1 ] ||
    fail "both polarity arms were skipped, so this run measured nothing about the probe's answer"
pass "at least one polarity arm ran"

# ---------------------------------------------------------------------------
# Teardown. The probe runs inside the export base of a verification lane, so
# litter it leaves becomes input to something else.
# ---------------------------------------------------------------------------

clean_dir="$(mktemp -d "${TMPDIR:-/tmp}/ee-appledouble-probe-clean.XXXXXX")"
before="$(ls -A "$clean_dir" | wc -l | tr -d ' ')"
probe_rc "$clean_dir" >/dev/null
probe_rc "$clean_dir" >/dev/null
after="$(ls -A "$clean_dir" | wc -l | tr -d ' ')"
[ "$before" = "$after" ] ||
    fail "the probe left entries behind" "before=$before after=$after dir=$clean_dir"
pass "two consecutive probes leave the directory byte-count-of-entries unchanged"

# An entry the probe did not create must survive. `rmdir` cannot remove a
# non-empty directory and the teardown uses `unlink`, never a recursive remove,
# so this is structural rather than a convention.
keep_dir="$(mktemp -d "${TMPDIR:-/tmp}/ee-appledouble-probe-keep.XXXXXX")"
printf 'keep me\n' >"$keep_dir/sentinel"
probe_rc "$keep_dir" >/dev/null
[ -f "$keep_dir/sentinel" ] || fail "the probe destroyed a file it did not create"
pass "a pre-existing sibling file survives a probe"

# Stdout must stay empty: every caller uses the function directly in `if`, and
# the guard in rch_verify.sh reads nothing from it.
stdout_capture="$(ee_dir_creates_appledouble_sidecars "$REPO_ROOT" 2>/dev/null || true)"
[ -z "$stdout_capture" ] ||
    fail "the probe wrote to stdout" "got: $stdout_capture"
pass "the probe is silent on stdout"

# ---------------------------------------------------------------------------
# THE GUARD THAT CONSUMES THE PROBE (bd-r4iwj).
#
# Everything above tests the probe. Nothing tested
# `assert_committed_tree_base_stores_xattrs` in scripts/rch_verify.sh: its
# Darwin gate, its deliberately narrow pinned-only scope, the degraded code it
# emits, or that it refuses rather than warns. A probe that answers correctly
# inside a guard that never calls it is worth nothing, so the guard is driven
# here directly.
#
# The function is extracted with awk and `emit_json` is stubbed -- the same
# technique tests/verification_drift_guard.rs uses on verify.sh's stages. That
# keeps the real guard body, including its own `exit 1`, which is why each arm
# runs in a subshell.
# ---------------------------------------------------------------------------

RCH_VERIFY="$REPO_ROOT/scripts/rch_verify.sh"

# Vacuity guard first: an awk range that matched nothing would define no
# function and every arm below would "pass" on a command-not-found.
for fn in resolve_committed_tree_export_base assert_committed_tree_base_stores_xattrs; do
    grep -q "^${fn}() {" "$RCH_VERIFY" ||
        fail "rch_verify.sh must define $fn at column 0 for the awk extraction to find it"
done
pass "both extracted functions are defined at column 0 in rch_verify.sh"

# Runs the real guard against one base. Echoes "EMIT <codes>" if it refused.
guard_against() {
    local base="$1" pinned="$2"
    (
        set -uo pipefail
        export SCRIPT_DIR="$REPO_ROOT/scripts"
        eval "$(awk '/^resolve_committed_tree_export_base\(\) /,/^}/' "$RCH_VERIFY")"
        eval "$(awk '/^assert_committed_tree_base_stores_xattrs\(\) /,/^}/' "$RCH_VERIFY")"
        emit_json() { printf 'EMIT %s\n' "${*: -1}"; }
        RCH_INVOCATION=()
        PINNED_FRANKEN_STACK="$pinned"
        RCH_VERIFY_COMMITTED_TREE_BASE="$base"
        assert_committed_tree_base_stores_xattrs
        printf 'RETURNED 0\n'
    )
}

if [ -n "$shim_point" ]; then
    guard_dir="$(mktemp -d "$shim_point/.ee-guard-arm.XXXXXX")"

    out="$(guard_against "$guard_dir" 1 || true)"
    case "$out" in
        *"EMIT rch_verify_committed_tree_base_appledouble_shim"*)
            pass "the guard REFUSES a pinned dispatch whose base shims xattrs" ;;
        *) fail "the guard did not refuse a shimming base" "got: $out" ;;
    esac

    # THE SCOPE ARM. Refusing is deliberately limited to the pinned lane: the
    # plain --committed-tree default base is ${TMPDIR:-/tmp}/..., which on this
    # host IS the shimming volume, so refusing there would break a rail nobody
    # has shown to be broken. Without this arm that scope looks like an
    # oversight and gets "fixed".
    out="$(guard_against "$guard_dir" 0 || true)"
    case "$out" in
        "RETURNED 0") pass "the guard is SILENT off the pinned lane, on the same hazardous base" ;;
        *) fail "the guard fired off the pinned lane; its scope is pinned-only on purpose" "got: $out" ;;
    esac

    rmdir "$guard_dir" 2>/dev/null ||
        printf 'SKIP guard probe dir not empty, left in place: %s\n' "$guard_dir"
else
    printf 'SKIP no xattr-shimming mount point; the guard refusal arm did not run\n'
fi

if [ -n "$native_point" ]; then
    native_guard_dir="$(mktemp -d "${TMPDIR:-/tmp}/ee-guard-arm-native.XXXXXX")"
    # Only meaningful if this temp dir is itself xattr-native; on this host
    # TMPDIR is the shimming volume, so check rather than assume.
    if [ "$(probe_rc "$native_guard_dir")" = "1" ]; then
        out="$(guard_against "$native_guard_dir" 1 || true)"
        case "$out" in
            "RETURNED 0") pass "the guard PASSES a pinned dispatch whose base stores xattrs natively" ;;
            *) fail "the guard refused an xattr-native base" "got: $out" ;;
        esac
    else
        printf 'SKIP %s is not xattr-native; the guard acceptance arm did not run\n' "$native_guard_dir"
    fi
    rmdir "$native_guard_dir" 2>/dev/null || true
else
    printf 'SKIP no xattr-native mount point; the guard acceptance arm did not run\n'
fi

printf '\nappledouble_sidecar_probe self-test: %s of %s arms passed\n' "$ARMS" "$ARMS"
