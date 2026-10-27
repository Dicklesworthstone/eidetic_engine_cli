#!/usr/bin/env bash
# Does a directory's filesystem shim extended attributes into AppleDouble
# `._*` sidecar files?
#
# WHY THIS EXISTS (bd-r4iwj). The pinned RCH lane materializes a committed-tree
# export with `git archive --format=tar | tar -x`. When that export lands on a
# filesystem that cannot store extended attributes natively -- ExFAT, FAT,
# anything mounted without xattr support -- macOS does not fail the write. It
# writes a companion `._<name>` file per entry to hold what the filesystem
# cannot. One measured export root held 6071 of them.
#
# Two correct rules then collide. `.rchignore:203-206` excludes `._*` from the
# transfer on purpose, because vendored C crates such as zstd-sys will compile
# any `._foo.c` sibling and poison the build (bd-1h8ji.4). rch's dependency-
# closure preflight enumerates required package source entrypoints from the
# LOCAL export and demands each one REMOTELY. Both halves are derived locally,
# so NO worker can satisfy it: every dispatch is refused
#
#   [RCH-E410] missing source_entrypoint .../benches/._adaptive_budget.rs
#
# in about 29 seconds, after about 4 minutes of export, and rch labels it
# `— retryable`. It is not. A whole session can go into retrying it.
#
# THE XATTR WRITE IS NOT THE DISCRIMINATOR, and this is the trap a simpler
# probe falls into. Measured 2026-10-25 on both filesystems: `xattr -w`
# returned 0 on APFS *and* on ExFAT. It succeeds on ExFAT precisely BECAUSE the
# shim writes the sidecar. A guard keyed on the write's exit status therefore
# passes the hazardous filesystem and detects nothing. The sidecar's existence
# is the only reading that separates the two.
#
# This probe shares the mechanism with the failure rather than modelling it: it
# writes a real extended attribute and looks for a real sidecar. It does not
# consult the filesystem's NAME, because a name-based predicate needs a
# whitelist it cannot keep current for network and image-backed mounts.

# ee_dir_creates_appledouble_sidecars <directory>
#
#   exit 0 -> writing an xattr HERE produced a `._*` sidecar: the hazard is
#             present on this filesystem.
#   exit 1 -> no sidecar appeared: this filesystem stores xattrs natively.
#   exit 2 -> indeterminate; the probe could not run. A diagnostic goes to
#             stderr. Callers should NOT refuse on 2 -- see the note below.
#
# Silent on stdout in every case, so a caller can use it directly in `if`.
#
# WHY 2 IS NOT A REFUSAL. Treating "cannot tell" as "hazardous" would refuse
# every dispatch on any host where /usr/bin/xattr is absent or the base is not
# yet writable, i.e. it would disable the lane outright. The cost of failing
# open is bounded and documented: you get the E410 refusal above, whose cause
# and fix are both in CLAUDE.md. The cost of failing closed is the whole lane.
ee_dir_creates_appledouble_sidecars() {
    local dir="${1:-}"

    if [ -z "$dir" ]; then
        printf 'ee_dir_creates_appledouble_sidecars: no directory given\n' >&2
        return 2
    fi
    if [ ! -d "$dir" ]; then
        printf 'ee_dir_creates_appledouble_sidecars: not a directory: %s\n' "$dir" >&2
        return 2
    fi
    if [ ! -x /usr/bin/xattr ]; then
        printf 'ee_dir_creates_appledouble_sidecars: /usr/bin/xattr unavailable; cannot probe %s\n' \
            "$dir" >&2
        return 2
    fi

    local probe_dir probe_file sidecar verdict
    probe_dir="$(mktemp -d "$dir/.ee-xattr-probe.XXXXXX" 2>/dev/null)" || {
        printf 'ee_dir_creates_appledouble_sidecars: cannot create a probe directory under %s\n' \
            "$dir" >&2
        return 2
    }
    probe_file="$probe_dir/p"
    sidecar="$probe_dir/._p"

    if ! : >"$probe_file" 2>/dev/null; then
        rmdir "$probe_dir" 2>/dev/null
        printf 'ee_dir_creates_appledouble_sidecars: cannot create a probe file under %s\n' \
            "$dir" >&2
        return 2
    fi

    /usr/bin/xattr -w com.apple.ee_rch_probe 1 "$probe_file" >/dev/null 2>&1 || true

    if [ -e "$sidecar" ]; then
        verdict=0
    else
        verdict=1
    fi

    # Teardown with `unlink` and `rmdir`, never a recursive remove: neither can
    # descend, and rmdir refuses a directory holding anything this probe did
    # not create. An unexpected entry is therefore LEFT IN PLACE and announced
    # rather than silently destroyed.
    [ -e "$sidecar" ] && unlink "$sidecar" 2>/dev/null
    unlink "$probe_file" 2>/dev/null
    if ! rmdir "$probe_dir" 2>/dev/null; then
        printf 'ee_dir_creates_appledouble_sidecars: probe directory not empty, left in place: %s\n' \
            "$probe_dir" >&2
    fi

    return "$verdict"
}
