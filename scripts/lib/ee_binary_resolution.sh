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
# Modification time and size of a candidate binary, as observability beside the
# version. Neither failure is fatal -- a missing timestamp must never turn a
# usable binary into a refusal.
#
# GNU IS TRIED FIRST, AND THE ORDER IS THE WHOLE FIX. The first version of
# these helpers put the BSD form first with the GNU form as a `||` fallback,
# which looks like ordinary portability and is not: on GNU coreutils `-f` means
# "file system status", NOT "format". So `stat -f '%Sm' -t ... FILE` SUCCEEDS on
# Linux and prints the FILESYSTEM's statistics, the `||` never fires, and the
# field carries the wrong data with exit 0. Measured on an RCH worker
# 2026-10-13, where the provenance line read
#
#     mtime=/data/projects/eidetic_engine_cli/.rch-target/debug/ee \
#     898458b21e9b3581 255 ef53 4096 4096 202873593 ...
#
# -- a device id, a namelen and block counts where a timestamp belongs, spread
# over five lines. A fallback chain only works when the wrong form FAILS on the
# other platform; `stat -c` does fail on BSD ("illegal option"), so this order
# degrades correctly in both directions.
#
# Both now emit EPOCH SECONDS rather than a formatted date. GNU `%y` renders
# "2026-10-13 17:59:08.837009596 -0400", whose spaces break the single-line
# provenance record every caller greps, and an integer is what a staleness
# comparison actually wants.
#
# The value is VALIDATED, not trusted, which is the second defence and the one
# that makes the order non-load-bearing. This repo already had two correct
# implementations when I wrote a third one wrong:
# scripts/beads_export_repair.sh:77-80 does GNU-first plus a digit check, and
# scripts/e2e_overhaul/workspace_hygiene.sh:532-536 tries BSD first but
# accepts the answer only if it matches ^[0-9]+$, so GNU's filesystem output
# falls through instead of being printed as a time. Either guard alone would
# have caught this; the version I shipped had neither.
ee_binary_mtime() {
    local binary="${1:?ee_binary_mtime: binary path required}"
    local value
    value="$(stat -c '%Y' "$binary" 2>/dev/null \
        || stat -f '%m' "$binary" 2>/dev/null || true)"
    case "$value" in
        '' | *[!0-9]*) printf 'unknown' ;;
        *) printf '%s' "$value" ;;
    esac
}

ee_binary_size() {
    local binary="${1:?ee_binary_size: binary path required}"
    local value
    value="$(stat -c '%s' "$binary" 2>/dev/null \
        || stat -f '%z' "$binary" 2>/dev/null || true)"
    case "$value" in
        '' | *[!0-9]*) printf 'unknown' ;;
        *) printf '%s' "$value" ;;
    esac
}

# ---------------------------------------------------------------------------
# WHY THIS GUARD DOES NOT COMPARE MTIME AGAINST THE SOURCE TREE
#
# The obvious strengthening -- refuse a binary older than the newest .rs file
# -- was proposed on bd-z6g1s and then rejected, because it would fail in the
# one environment this guard actually runs in.
#
# A `rch exec --job` executes in a CLEAN OVERLAY with no `.git` directory, so
# the tracked-file set cannot be enumerated with git there; and the overlay's
# sources are freshly materialised, so every .rs file carries a NEW mtime. A
# `--job` also inherits the worker's own target directory and legitimately
# reuses a previous job's binary (that is the documented fast path: the same
# build measured at 32 minutes rather than 217). Combine the two and the
# common, correct case -- freshly synced sources, validly cached binary --
# looks exactly like staleness. The guard would refuse good builds and wedge
# every gate that calls it.
#
# A guard that fails closed on its own happy path is worse than the hole it
# closes, so the within-version staleness hole stays OPEN and visible in the
# log line above rather than being closed by a predicate that cannot tell
# "stale" from "cached". Closing it properly needs build-commit identity
# stamped into the binary, which is its own piece of work.
# ---------------------------------------------------------------------------
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
    # mtime and size accompany the versions because SEMVER EQUALITY IS NOT
    # COMMIT IDENTITY: this guard passes for any binary declaring the same
    # version, however stale within that version. It cannot refuse on that
    # basis (see the note below), so it must at least leave enough in the log
    # for a reader to notice. mtime+size are O(1); sha256 is deliberately not
    # printed here even though ee_binary_sha256 exists, because a debug `ee` is
    # ~2 GB and this runs five-plus times per Gate 8.5.
    printf '%s: ee_binary=%s binary_version=%s source_version=%s mtime_epoch=%s size_bytes=%s\n' \
        "$label" "$binary" "${binary_version:-unknown}" "${source_version:-unknown}" \
        "$(ee_binary_mtime "$binary")" "$(ee_binary_size "$binary")" >&2

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

# Compare the candidate binary's BUILD target against this host's target
# (bd-reality-core-convergence-1azkt.5, bullet 5, "target").
#
# ee_binary_executes_here already refuses a binary whose FORMAT is foreign to
# this host. That is a different question: a binary cross-built for a sibling
# triple can still load and run, and then every stage reports on a build nobody
# asked for. "Can execute here" is not "was built for here".
#
# Exit codes are distinct because "unknown" is NOT a mismatch. A binary built
# without EE_BUILD_TARGET reports "unknown" -- src/core/mod.rs already models
# that as the `target_triple_unavailable` degradation -- and calling it a
# mismatch sends a reader hunting a cross-compile that never happened.
#   0  match
#   1  mismatch -- prints both triples
#   2  the binary reports "unknown": a missing fact, not a conflict
#   3  either side is empty, so there is nothing to compare
ee_assert_target_triple_matches_host() {
    local actual="${1-}"
    local host="${2-}"
    local binary="${3:-the candidate binary}"
    local label="${4:-verify}"

    if [ -z "$actual" ] || [ -z "$host" ]; then
        printf '%s: target triple NOT CHECKED: actual=%s host=%s\n' \
            "$label" "${actual:-<empty>}" "${host:-<empty>}" >&2
        return 3
    fi

    if [ "$actual" = "unknown" ]; then
        printf '%s: candidate binary reports target_triple=unknown.\n' "$label" >&2
        printf '%s: it was built without EE_BUILD_TARGET, so WHICH TARGET it was\n' "$label" >&2
        printf '%s: built for cannot be established. A missing fact, not a mismatch.\n' "$label" >&2
        return 2
    fi

    if [ "$actual" = "$host" ]; then
        printf '%s: candidate binary target matches host: %s\n' "$label" "$actual" >&2
        return 0
    fi

    printf '%s: candidate binary was built for a different target.\n' "$label" >&2
    printf '%s:   host   (rustc -vV): %s\n' "$label" "$host" >&2
    printf '%s:   binary (%s): %s\n' "$label" "$binary" "$actual" >&2
    printf '%s: executing here is not the same as being built for here; stages\n' "$label" >&2
    printf '%s: would report on a build this host did not ask for.\n' "$label" >&2
    return 1
}

# Is the candidate binary's build OS one the manifest declares support for?
# (bd-reality-core-convergence-1azkt.5, bullet 1, "platform".)
#
# The declared list in verify-budget.toml `[requirements].supported_target_os`
# is the SOURCE of this fact, not a copy of one, which is why a list is the right
# shape here -- nothing else in the tree states where the manifest is valid.
#
#   0  supported
#   1  not supported -- prints the actual and the declared set
#   3  either side empty, so there is nothing to decide
ee_assert_target_os_supported() {
    local actual="${1-}"
    local supported="${2-}"
    local label="${3:-verify}"
    local entry

    if [ -z "$actual" ] || [ -z "$supported" ]; then
        printf '%s: target OS NOT CHECKED: actual=%s supported=%s\n' \
            "$label" "${actual:-<empty>}" "${supported:-<empty>}" >&2
        return 3
    fi

    # Space-padded token match rather than `for entry in $supported`. This file
    # is SOURCED, and zsh does not word-split an unquoted parameter, so the loop
    # form iterates once over the whole string and refuses every OS -- failing
    # closed, but with a message that blames the manifest for a shell
    # difference. Padding both sides makes this exact-token matching, not
    # substring matching: " linu " does not occur in " ... linux ".
    case " $supported " in
        *" $actual "*)
            printf '%s: target OS %s is declared supported.\n' "$label" "$actual" >&2
            return 0
            ;;
    esac

    printf '%s: candidate binary target OS is not declared supported.\n' "$label" >&2
    printf '%s:   binary target_os: %s\n' "$label" "$actual" >&2
    printf '%s:   manifest declares: %s\n' "$label" "$supported" >&2
    printf '%s: verify-budget.toml [requirements].supported_target_os says where\n' "$label" >&2
    printf '%s: this manifest is valid. Running it elsewhere reports on a\n' "$label" >&2
    printf '%s: platform nobody declared it covers.\n' "$label" >&2
    return 1
}

# Does the candidate binary's enabled feature set match what Cargo.toml declares
# as `default = [...]`? (bd-reality-core-convergence-1azkt.5, bullet 5,
# "features"; emptiness arm from bd-sb2pl.)
#
# WHY THIS MOVED OUT OF verify.sh. The comparison was an inline `[ "$a" != "$b" ]`
# there, which that file's own comment on the TRIPLE check already argues against:
# "the comparison lives in the resolution lib so it has self-test arms; an inline
# chain here could only be exercised by running the whole script." Measured
# 2026-10-18: of this suite's 36 arms, 0 exercised the feature comparison, while
# the three sibling properties (hash, triple, OS) had 4, 4 and 4.
#
# THE TWO SIDES ARE NOT SYMMETRIC, which is why this does not reuse the triple
# helper's single `-z a || -z b` arm:
#
#   EXPECTED empty is never legitimate. `default = []` MATCHES verify.sh's regex,
#   so findall returns [], the helper prints an empty line and exits 0 -- a
#   success status carrying no expectation. Measured against a `default = []`
#   fixture 2026-10-18. The old exit-status guard passed it, "" == "" compared
#   equal, and the check certified a binary it had not examined.
#
#   ACTUAL empty IS legitimate: a binary built --no-default-features genuinely
#   has zero features enabled. Against a non-empty expectation that is a real
#   MISMATCH (1), not an unreadable world (3) -- the distinction the triple
#   helper does not have to draw because ee_binary_target_triple refuses an
#   empty triple before returning.
#
# Equality of the sorted, space-joined lists, because both callers sort.
#
#   0  match
#   1  mismatch -- prints both sets, rendering an empty side as <none>
#   3  no expectation to compare against, so nothing was checked
ee_assert_feature_set_matches_manifest() {
    local actual="${1-}"
    local expected="${2-}"
    local binary="${3:-the candidate binary}"
    local label="${4:-verify}"

    if [ -z "$expected" ]; then
        printf '%s: features NOT CHECKED: Cargo.toml declared no default set.\n' "$label" >&2
        printf '%s: `default = []` reads as success but carries no expectation,\n' "$label" >&2
        printf '%s: and a binary with no features enabled would compare equal to\n' "$label" >&2
        printf '%s: it. Two empty values are not a match.\n' "$label" >&2
        return 3
    fi

    if [ "$actual" = "$expected" ]; then
        printf '%s: candidate binary features match Cargo.toml default: %s\n' \
            "$label" "$actual" >&2
        return 0
    fi

    printf '%s: candidate binary feature set does not match Cargo.toml.\n' "$label" >&2
    printf '%s:   expected (Cargo.toml default): %s\n' "$label" "$expected" >&2
    printf '%s:   actual   (%s): %s\n' "$label" "$binary" "${actual:-<none>}" >&2
    printf '%s: a stage needing an optional feature does not error at the feature\n' "$label" >&2
    printf '%s: boundary -- it takes the fallback path and asserts against that.\n' "$label" >&2
    return 1
}

# Default binary for a harness that would otherwise fall back to PATH `ee`
# (bd-z6g1s).
#
# Prints a provenance line naming the path AND the origin, then echoes the
# path. The origin is not decoration: the three cases fail in different ways
# and a log that records only the path cannot tell them apart afterwards.
#
#   caller        an explicit binary was supplied -- $2, else
#                 EE_DOCTOR_FIXTURE_BINARY, else EE_BINARY. This is what
#                 run-safety-harness.sh does, so Gate 8.5 always lands here
#                 and this helper's fallbacks never apply to it.
#   built         the binary this checkout built, and it executes here.
#   path_fallback the built binary is absent or cannot execute here, so PATH
#                 `ee` is used instead.
#
# $2 exists because callers disagree about the name of their override: the five
# verify-* harnesses use EE_DOCTOR_FIXTURE_BINARY, verify.sh exports EE_BINARY,
# and scripts/e2e_delivery.sh uses EE_BIN. Passing the value in keeps the
# provenance line unconditional -- a caller that short-circuited this function
# to honour its own variable would resolve correctly and print nothing, which
# is the exact failure this whole bead is about.
#
# The executability probe is the load-bearing part, and preferring the built
# binary WITHOUT it is a regression rather than a fix. Measured on the Mac dev
# host 2026-10-13: `ee_resolve_binary debug` resolved a 2.1 GB Linux x86-64
# ELF left in the shared Cargo target directory by an RCH run. It satisfied
# `[ -x ]` and `command -v`, so a resolver that stopped there would hand every
# harness a binary that dies with `exec format error` -- strictly worse than
# the PATH binary it replaced, which at least runs. See `ee_binary_executes_here`.
ee_resolve_fixture_binary() {
    local label="${1:?ee_resolve_fixture_binary: label required}"
    local candidate="${2:-${EE_DOCTOR_FIXTURE_BINARY:-${EE_BINARY:-}}}"
    local origin=caller

    if [ -z "$candidate" ]; then
        candidate="$(ee_resolve_binary debug 2>/dev/null || true)"
        origin=built
        if [ -z "$candidate" ] || [ ! -x "$candidate" ] \
            || ! ee_binary_executes_here "$candidate"; then
            candidate="ee"
            origin=path_fallback
        fi
    fi

    printf '%s: ee_binary=%s origin=%s\n' "$label" "$candidate" "$origin" >&2
    printf '%s\n' "$candidate"
}
