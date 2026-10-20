#!/usr/bin/env bash
set -euo pipefail

doctor_fixture_target() {
    if [ -n "${EE_DOCTOR_FIXTURE_TARGET:-}" ]; then
        printf '%s\n' "$EE_DOCTOR_FIXTURE_TARGET"
        return 0
    fi
    printf 'EE_DOCTOR_FIXTURE_TARGET is required\n' >&2
    return 2
}

doctor_fixture_marker_dir() {
    local target="${1:?target required}"
    printf '%s\n' "$target/.ee/doctor-fixtures"
}

doctor_fixture_content_digest() {
    local target="${1:?target required}"
    # Exclude the doctor's own audit trail, the fixture's baseline directory,
    # the test wrapper's assert.* capture files, and macOS HFS+/ExFAT resource
    # fork sidecars (the `._*` files that appear on non-HFS volumes).
    #
    # Two runtime files are CLASSIFIED rather than byte-compared (bd-2oh15,
    # orchestrator decision on c9818). A real doctor fix + undo changed exactly
    # these two outside .doctor/:
    # - `ee.db-shm` is excluded: a transient SQLite shared-memory index derived
    #   from the WAL and rewritten by readers. It holds no source of truth.
    # - `ee.write.lock` is excluded from the BYTES but checked by its semantics
    #   (doctor_fixture_assert_write_lock_monotonic): a monotonic epoch counter
    #   that no honest undo can restore.
    # - `.doctor.lock` joins that class as of 2026-10-20 (bd-ooj2h). It is
    #   excluded from the BYTES and checked by its semantics
    #   (doctor_fixture_assert_doctor_lock_persistent).
    # Everything else, including ee.db and ee.db-wal, stays byte-compared.
    #
    # WHY `.doctor.lock` MOVED, because this line used to say the opposite.
    # The c9818 decision byte-compared it on the observation that "a real doctor
    # fix + undo changed exactly these two outside .doctor/". That observation no
    # longer holds, and the product documents why it cannot:
    #
    #   src/core/doctor_runtime.rs:9     "an OS advisory lock on the persistent
    #                                     <workspace>/.ee/.doctor.lock file"
    #   src/core/doctor_runtime.rs:82    DOCTOR_LOCK_FILE_MARKER =
    #                                     "ee.doctor.persistent_lock.v1\n"
    #   src/core/doctor_runtime.rs:3643  "The `.doctor.lock` pathname is
    #                                     persistent and may now name a peer
    #                                     replacement, so teardown must never
    #                                     inspect, unlink, overwrite, or rename
    #                                     it."
    #
    # So a baseline captured before any doctor run cannot contain the lock, the
    # first `--fix` creates it, and `--undo` is FORBIDDEN from removing it --
    # unlinking it could destroy a concurrent peer's replacement lock. A
    # byte-identical assertion over it therefore demands behaviour the product
    # is explicitly designed never to perform. Measured 2026-10-20 on an RCH
    # worker against ee 0.18.0: both of this bead's COVERAGE failures were this
    # one file, same digest 812ab0a5, and nothing else differed:
    #
    #   verify-undo[fm-state_files-merge-conflict-markers]:   14a15 > .ee/.doctor.lock
    #   verify-undo[fm-workspace_config-config-toml-malformed]: 22a23 > .ee/.doctor.lock
    #
    # This is a correction, NOT a relaxation: the file is still asserted, by the
    # contract it actually has. Do not "restore" the byte comparison without
    # first changing doctor_runtime's persistence contract.
    doctor_fixture_content_manifest "$target" | shasum -a 256
}

# The PER-FILE listing the digest above rolls up. Same exclusions, same sort,
# emitted one `<hash>  <path>` line per file instead of a single aggregate.
#
# bd-2oh15: the digest is a single rollup, so a failed `cmp` of two digests can
# say THAT the tree differs and never WHICH file differs -- the information is
# destroyed by the final shasum. That is why this bead's two COVERAGE failures
# ("undo contents differ from the baseline" for
# fm-state_files-merge-conflict-markers and
# fm-workspace_config-config-toml-malformed) stayed unnamed across runs, each
# costing a fresh fleet dispatch to re-learn nothing new.
#
# Keeping both means the cheap aggregate stays the assertion while the manifest
# makes a failure self-explaining.
doctor_fixture_content_manifest() {
    local target="${1:?target required}"
    find "$target" -type f \
        -not -path '*/.doctor/*' \
        -not -path '*/.fixture_baseline/*' \
        -not -path '*/.ee/doctor-fixtures/*' \
        -not -path '*/.ee/ee.db-shm' \
        -not -path '*/.ee/ee.write.lock' \
        -not -path '*/.ee/.doctor.lock' \
        -not -name '.assert.stdout' \
        -not -name '.assert.stderr' \
        -not -name '._*' \
        -exec shasum -a 256 -- {} + | LC_ALL=C sort
}

# Print the flock-gate epoch stored in an ee.write.lock file, or fail. Format
# (src/db/mod.rs read_flock_gate_epoch): exactly 21 bytes, 20 ASCII digits,
# then a newline; a zero-padded decimal u64. Anything else is unreadable.
doctor_fixture_write_lock_epoch() {
    local lock="${1:?lock path required}"
    [ -f "$lock" ] && [ ! -L "$lock" ] || return 1
    [ "$(wc -c < "$lock" | tr -d ' ')" = "21" ] || return 1
    local digits
    digits="$(head -c 20 "$lock")"
    [[ "$digits" =~ ^[0-9]{20}$ ]] || return 1
    # The 21st byte must be the newline (command substitution strips it).
    [ -z "$(tail -c 1 "$lock")" ] || return 1
    printf '%s\n' "$digits"
}

doctor_fixture_record_write_lock() {
    local target="${1:?target required}"
    local lock="$target/.ee/ee.write.lock"
    if [ -e "$lock" ] || [ -L "$lock" ]; then
        if ! doctor_fixture_write_lock_epoch "$lock" > "$target/.fixture_baseline/write-lock.before"; then
            printf 'fixture corrupt: baseline ee.write.lock is unreadable: %s\n' "$lock" >&2
            return 1
        fi
    else
        printf 'absent\n' > "$target/.fixture_baseline/write-lock.before"
    fi
}

# ee.write.lock is a monotonic epoch: after undo it must still exist and its
# epoch must be >= the baseline. The fixed-width 20-digit encoding makes a string
# comparison exact. A lock absent at the baseline must stay absent: a created
# lock is a new path the byte digest no longer sees.
doctor_fixture_assert_write_lock_monotonic() {
    local fm_id="${1:?fm id required}"
    local target="${2:?target required}"
    local lock="$target/.ee/ee.write.lock"
    local before after
    before="$(cat "$target/.fixture_baseline/write-lock.before")"
    if [ "$before" = "absent" ]; then
        if [ -e "$lock" ] || [ -L "$lock" ]; then
            printf 'fixture assert: ee.write.lock was absent at the baseline but exists after undo for %s\n' \
                "$fm_id" >&2
            return 1
        fi
        return 0
    fi
    if ! after="$(doctor_fixture_write_lock_epoch "$lock")"; then
        printf 'fixture assert: ee.write.lock is missing or unreadable after undo for %s\n' "$fm_id" >&2
        return 1
    fi
    if [[ "$after" < "$before" ]]; then
        printf 'fixture assert: ee.write.lock epoch went backwards (%s -> %s) for %s\n' \
            "$before" "$after" "$fm_id" >&2
        return 1
    fi
}

# `.doctor.lock` replaces its byte comparison with this (bd-ooj2h). It is
# excluded from the digest because the product documents it as PERSISTENT and
# forbids teardown from touching it (src/core/doctor_runtime.rs:3643), so a
# byte-identical assertion over it demanded behaviour `ee doctor --undo` is
# designed never to perform. See the exclusion comment in
# doctor_fixture_content_digest for the measurement.
#
# This is the stronger assertion, not the weaker one. The byte comparison only
# said "same as the pre-fix baseline", and the baseline has NO lock at all, so
# it could only ever fail. This checks the contract the file actually has:
# after a fix + undo round trip the lock MUST still exist, and its contents MUST
# be exactly DOCTOR_LOCK_FILE_MARKER (src/core/doctor_runtime.rs:82).
#
# Verified 2026-10-20: the single file both COVERAGE failures reported had
# digest 812ab0a5..., and sha256 of the marker text is that same digest, so the
# residue is the marker byte for byte and nothing else differed.
#
# A marker version bump (v1 -> v2) SHOULD red this. That is a doctor runtime
# contract change and the fixture is the right place to notice it.
DOCTOR_LOCK_EXPECTED_MARKER='ee.doctor.persistent_lock.v1'

# Record whether the lock existed BEFORE the fix, mirroring
# doctor_fixture_record_write_lock. The baseline is what makes the assertion
# below able to distinguish "doctor never created a lock" from "undo removed
# one", and those must not be conflated:
#
#   absent -> absent   doctor created none. PASS. This is the shape the
#                      assertion_contract.py scenarios produce, because their
#                      fake `ee` never takes a real lock.
#   absent -> present  the normal real-fixture path. PASS, marker required.
#   present -> absent  undo unlinked a persistent lock. FAIL.
#   present -> present PASS, marker required.
doctor_fixture_record_doctor_lock() {
    local target="${1:?target required}"
    local lock="$target/.ee/.doctor.lock"
    if [ -e "$lock" ] || [ -L "$lock" ]; then
        printf 'present\n' > "$target/.fixture_baseline/doctor-lock.before"
    else
        printf 'absent\n' > "$target/.fixture_baseline/doctor-lock.before"
    fi
}

doctor_fixture_assert_doctor_lock_persistent() {
    local fm_id="${1:?fm id required}"
    local target="${2:?target required}"
    local lock="$target/.ee/.doctor.lock"
    local before="absent"
    # A missing baseline means doctor_fixture_corrupt predates this check;
    # treat it as `absent` rather than failing, so an old fixture tree does not
    # red on the bookkeeping instead of on doctor's behaviour.
    if [ -f "$target/.fixture_baseline/doctor-lock.before" ]; then
        before="$(cat "$target/.fixture_baseline/doctor-lock.before")"
    fi
    if [ ! -f "$lock" ]; then
        if [ "$before" = "present" ]; then
            printf 'fixture assert: .doctor.lock existed at the baseline but is absent after undo for %s; doctor_runtime:3643 forbids teardown unlinking it\n' \
                "$fm_id" >&2
            return 1
        fi
        return 0
    fi
    if ! grep -qF "$DOCTOR_LOCK_EXPECTED_MARKER" "$lock"; then
        printf 'fixture assert: .doctor.lock does not carry %s after undo for %s; undo overwrote a persistent lock\n' \
            "$DOCTOR_LOCK_EXPECTED_MARKER" "$fm_id" >&2
        return 1
    fi
}

doctor_fixture_corrupt() {
    local fm_id="${1:?fm id required}"
    local severity="${2:?severity required}"
    local subsystem="${3:?subsystem required}"
    local target
    target="$(doctor_fixture_target)"
    local marker_dir
    marker_dir="$(doctor_fixture_marker_dir "$target")"
    mkdir -p "$marker_dir" "$target/.fixture_baseline"
    # Hash each regular file's bytes AND its pathname, then hash the sorted
    # manifest. A same-name content change must not survive undo unnoticed.
    doctor_fixture_content_digest "$target" > "$target/.fixture_baseline/before.sha256"
    # bd-2oh15: keep the per-file listing beside the rollup so a later mismatch
    # can name the file instead of only reporting that one exists.
    doctor_fixture_content_manifest "$target" > "$target/.fixture_baseline/before.manifest"
    doctor_fixture_record_write_lock "$target"
    doctor_fixture_record_doctor_lock "$target"
    printf '{"schema":"ee.doctor_fixture_marker.v1","fmId":"%s","severity":"%s","subsystem":"%s","state":"corrupt"}\n' \
        "$fm_id" "$severity" "$subsystem" > "$marker_dir/$fm_id.json"
    printf 'corrupt fixture prepared: %s\n' "$fm_id" >&2
}

doctor_fixture_assert_health_report() {
    local fm_id="${1:?fm id required}"
    local report="${2:?report required}"
    # success means the command ran, not that the workspace is healthy.
    # The default JSON renderer exposes coreChecks (the full renderer uses
    # checks). Reject missing/empty populations and multiple JSON documents
    # as well as non-ok health, even when the process exits successfully.
    if ! jq -es '
        length == 1 and (.[0] |
            .schema == "ee.response.v2" and .success == true and
            .data.command == "doctor" and .data.posture == "ok" and
            .data.healthy == true and
            (.data.coreChecks | type == "array" and length > 0 and
                all(.[]; .tier == "core" and .severity == "ok")) and
            (.data.actionable | type == "array" and length == 0))
    ' "$report" >/dev/null; then
        printf 'fixture assert: post-fix health not established for %s; see %s\n' \
            "$fm_id" "$report" >&2
        return 1
    fi
}

# Guidance-only failure modes (bd-2oh15, orchestrator decision C): doctor's
# repair for these is not implemented or cannot safely run on the damaged
# source, so doctor records guidance instead of repairing. The contract is
# honesty, not repair: --fix must report the finding as guidance_recorded and
# never as applied, the finding must still be reported afterwards, and the path
# the real repair would create must still be absent.
doctor_fixture_assert_guidance_only() {
    local fm_id="${1:?fm id required}"
    local finding_code="${2:?finding code required}"
    local check_name="${3:?check name required}"
    local error_code="${4:?error code required}"
    local absent_rel="${5:?path that must stay absent required}"
    local target
    target="$(doctor_fixture_target)"
    test -f "$(doctor_fixture_marker_dir "$target")/$fm_id.json"
    if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" != "1" ]; then
        printf 'fixture assert: %s requires EE_DOCTOR_FIXTURE_RUN_EE=1; marker-only checks are insufficient\n' \
            "$fm_id" >&2
        return 2
    fi
    local ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
    local fix_exit=0
    "$ee_bin" doctor --workspace "$target" --fix --json > "$target/.fixture_baseline/doctor-fix.json" || fix_exit=$?
    if [ "$fix_exit" -ne 6 ]; then
        printf 'fixture assert: %s unresolved core guidance must exit 6, got %s\n' "$fm_id" "$fix_exit" >&2
        if [ "$fix_exit" -eq 0 ]; then return 1; fi
        return "$fix_exit"
    fi
    if ! jq -es --arg code "$finding_code" --arg check "$check_name" --arg error "$error_code" '
        length == 1 and (.[0] |
            .schema == "ee.response.v2" and .success == true and
            .data.status == "completed_partial" and .data.fixerDispatchPending == true and
            (.data.unresolvedCoreCheckCount | type == "number" and . >= 1) and
            (.data.unresolvedCoreChecks | type == "array" and
                any(.[]; .name == $check and .errorCode == $error)) and
            (.data.guidanceOnlyFixerCount | type == "number" and . >= 1) and
            (.data.fixerResults | type == "array") and
            any(.data.fixerResults[]; .findingCode == $code and .outcome == "guidance_recorded") and
            all(.data.fixerResults[]; .findingCode != $code or .outcome == "guidance_recorded"))
    ' "$target/.fixture_baseline/doctor-fix.json" >/dev/null; then
        printf 'fixture assert: %s fix did not record %s as guidance_recorded; see %s\n' \
            "$fm_id" "$finding_code" "$target/.fixture_baseline/doctor-fix.json" >&2
        return 1
    fi
    "$ee_bin" doctor --workspace "$target" --json > "$target/.fixture_baseline/doctor-after.json"
    if ! jq -es --arg check "$check_name" --arg error "$error_code" '
        length == 1 and (.[0] |
            .schema == "ee.response.v2" and .success == true and
            .data.healthy == false and
            any(.data.actionable[]; .name == $check and .errorCode == $error))
    ' "$target/.fixture_baseline/doctor-after.json" >/dev/null; then
        printf 'fixture assert: %s after guidance, doctor no longer reports %s %s; a real repair happened or the report is unreadable; see %s\n' \
            "$fm_id" "$check_name" "$error_code" "$target/.fixture_baseline/doctor-after.json" >&2
        return 1
    fi
    if [ -e "$target/$absent_rel" ] || [ -L "$target/$absent_rel" ]; then
        printf 'fixture assert: %s guidance-only fix created %s\n' "$fm_id" "$absent_rel" >&2
        return 1
    fi
    printf 'guidance-only fixture confirmed: %s (%s guidance_recorded, %s %s unchanged)\n' \
        "$fm_id" "$finding_code" "$check_name" "$error_code" >&2
}

doctor_fixture_assert() {
    local fm_id="${1:?fm id required}"
    local severity="${2:?severity required}"
    local subsystem="${3:?subsystem required}"
    local target
    target="$(doctor_fixture_target)"
    local marker_dir
    marker_dir="$(doctor_fixture_marker_dir "$target")"
    test -f "$marker_dir/$fm_id.json"

    if [ "${EE_DOCTOR_FIXTURE_RUN_EE:-0}" = "1" ]; then
        local ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
        # ee's CLI surface (bd-3boan): --fix and --undo <RUN_ID> are flags, not
        # subcommands. The --fix flag declares `conflicts_with --only`, so we
        # invoke --fix on its own and pass --only on the read-only diagnose
        # call. That selector is currently advisory in the CLI; it does not
        # establish per-FM detector coverage. Assert the reported core health.
        "$ee_bin" doctor --workspace "$target" --fix --json > "$target/.fixture_baseline/doctor-fix.json"
        "$ee_bin" doctor --workspace "$target" --only "$fm_id" --json > "$target/.fixture_baseline/doctor-after.json"
        doctor_fixture_assert_health_report "$fm_id" "$target/.fixture_baseline/doctor-after.json"
        local run_id
        run_id="$(jq -r '.runId // .data.runId // empty' "$target/.fixture_baseline/doctor-fix.json")"
        if [ -z "$run_id" ]; then
            printf 'fixture assert: could not extract runId from fix output\n' >&2
            cat "$target/.fixture_baseline/doctor-fix.json" >&2
            return 1
        fi
        "$ee_bin" doctor --workspace "$target" --undo "$run_id" --json > "$target/.fixture_baseline/doctor-undo.json"
        doctor_fixture_content_digest "$target" > "$target/.fixture_baseline/after-undo.sha256"
        doctor_fixture_content_manifest "$target" > "$target/.fixture_baseline/after-undo.manifest"
        if ! cmp "$target/.fixture_baseline/before.sha256" "$target/.fixture_baseline/after-undo.sha256"; then
            printf 'fixture assert: undo contents differ from the baseline for %s\n' "$fm_id" >&2
            # bd-2oh15: NAME THE FILES. The digests above are rollups, so the
            # line before this one can only report that something differs. The
            # manifests are the same data pre-aggregation, so diffing them says
            # which paths changed and in which direction:
            #   `<` only in the pre-fix baseline, so undo failed to restore it
            #   `>` only after undo, so undo left something behind
            # A path on BOTH sides changed content rather than existence.
            printf 'fixture assert: differing entries (< baseline, > after undo):\n' >&2
            diff "$target/.fixture_baseline/before.manifest" \
                 "$target/.fixture_baseline/after-undo.manifest" >&2 || true
            return 1
        fi
        doctor_fixture_assert_write_lock_monotonic "$fm_id" "$target" || return 1
        doctor_fixture_assert_doctor_lock_persistent "$fm_id" "$target" || return 1
    fi

    printf 'assert fixture ready: %s %s %s\n' "$fm_id" "$severity" "$subsystem" >&2
}

# ---------------------------------------------------------------------------
# Real-corruption building blocks (bd-2oh15 P0 tranche). MOVES ONLY: an original
# is moved into .fixture_baseline before a damaged version is written as a NEW
# file. Nothing is deleted, and nothing is overwritten in place.
# ---------------------------------------------------------------------------

# Refuse a symlink or non-empty target rather than touching somebody's state.
doctor_fixture_prepare_target() {
    local target="${1:?target required}"
    if [ -L "$target" ]; then
        printf 'fixture refuses a symlink target: %s\n' "$target" >&2
        return 2
    fi
    mkdir -p "$target"
    if [ -n "$(find "$target" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
        printf 'fixture requires an empty target: %s\n' "$target" >&2
        return 2
    fi
    mkdir -p "$target/.fixture_baseline"
}

# A real store with one source memory and a populated index, asserted healthy
# before any damage. A failure mode is only meaningful against a real baseline.
doctor_fixture_healthy_store() {
    local fm_id="${1:?fm id required}"
    local target="${2:?target required}"
    local ee_bin="${3:?ee binary required}"
    local base="$target/.fixture_baseline"
    "$ee_bin" --workspace "$target" init --skip-boilerplate --json > "$base/init.json"
    "$ee_bin" remember "Preserve this source memory while $fm_id is exercised." \
        --workspace "$target" --level procedural --kind rule --json > "$base/remember.json"
    "$ee_bin" index rebuild --workspace "$target" --json > "$base/index-healthy.json"
    jq -e '.schema == "ee.response.v2" and .success == true and .data.memories_indexed >= 1' \
        "$base/index-healthy.json" >/dev/null
    "$ee_bin" doctor --workspace "$target" --json > "$base/doctor-healthy.json"
    doctor_fixture_assert_health_report "$fm_id" "$base/doctor-healthy.json"
}

# Write <dst> as a NEW file: <src> with one 4096-byte page replaced by random
# bytes. <src> is read, never modified.
doctor_fixture_damaged_copy() {
    local src="${1:?source required}"
    local dst="${2:?destination required}"
    local page="${3:?page index required}"
    local off=$(( page * 4096 ))
    {
        head -c "$off" "$src"
        head -c 4096 /dev/urandom
        tail -c +"$(( off + 4096 + 1 ))" "$src"
    } > "$dst"
}

doctor_fixture_sha256() {
    shasum -a 256 -- "${1:?path required}" | cut -d' ' -f1
}

# NOT-DETECTED contract (orchestrator decision, bd-2oh15): a PINNED GAP.
# doctor reports the damaged workspace healthy, --fix takes no action, and the
# damaged artifact is byte-identical afterwards. The fixture's own assert.sh
# must separately prove the damage is real with an independent witness. The day
# doctor starts detecting this FM, this assertion FAILS on purpose: upgrade the
# spec label and the fixture together (spec rule V6). Pinned gaps never count
# toward coverage; the harness reports them as a separate GAPS count.
# <artifact_rel> may be '-' when the damage is not a single file.
doctor_fixture_assert_pinned_gap() {
    local fm_id="${1:?fm id required}"
    local artifact_rel="${2:?artifact path or - required}"
    shift 2
    local target
    target="$(doctor_fixture_target)"
    local ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
    local base="$target/.fixture_baseline"
    local before=""
    if [ "$artifact_rel" != "-" ]; then
        before="$(doctor_fixture_sha256 "$target/$artifact_rel")"
    fi
    "$ee_bin" doctor --workspace "$target" "$@" --json > "$base/gap-doctor.json"
    if ! jq -es '
        length == 1 and (.[0] |
            .schema == "ee.response.v2" and .success == true and
            .data.healthy == true and .data.posture == "ok" and
            (.data.actionable | type == "array" and length == 0))
    ' "$base/gap-doctor.json" >/dev/null; then
        printf 'fixture assert: %s is a PINNED GAP but doctor now reports something; upgrade the spec and fixture (V6); see %s\n' \
            "$fm_id" "$base/gap-doctor.json" >&2
        return 1
    fi
    "$ee_bin" doctor --workspace "$target" --fix --json > "$base/gap-fix.json"
    if ! jq -es '
        length == 1 and (.[0] |
            .schema == "ee.response.v2" and .success == true and
            .data.actionCount == 0 and (.data.fixerResults | length) == 0)
    ' "$base/gap-fix.json" >/dev/null; then
        printf 'fixture assert: %s is a PINNED GAP but --fix acted; upgrade the spec and fixture (V6); see %s\n' \
            "$fm_id" "$base/gap-fix.json" >&2
        return 1
    fi
    if [ -n "$before" ] && [ "$(doctor_fixture_sha256 "$target/$artifact_rel")" != "$before" ]; then
        printf 'fixture assert: %s damaged artifact %s changed during doctor/--fix\n' \
            "$fm_id" "$artifact_rel" >&2
        return 1
    fi
    printf 'pinned gap confirmed: %s (doctor healthy, --fix 0 actions, damage intact) -- NOT coverage\n' \
        "$fm_id" >&2
}

# REPORT-ONLY contract: doctor detects the FM (check + error code), but the
# dispatch table has no entry for it, so --fix takes no action and records no
# fixer result. The damage is untouched and still reported afterwards.
# <view> is 'concise' (actionable[]) or 'full' (every check, --full).
doctor_fixture_assert_report_only() {
    local fm_id="${1:?fm id required}"
    local check_name="${2:?check name required}"
    local error_code="${3:?error code required}"
    local view="${4:?concise or full required}"
    local artifact_rel="${5:?artifact path or - required}"
    local target
    target="$(doctor_fixture_target)"
    local ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
    local base="$target/.fixture_baseline"
    local before="" doctor_args=(--json) filter
    if [ "$view" = "full" ]; then
        doctor_args=(--full --json)
        filter='any(.. | objects | select(has("name") and has("errorCode")); .name == $check and .errorCode == $code)'
    else
        filter='any(.data.actionable[]?; .name == $check and .errorCode == $code)'
    fi
    if [ "$artifact_rel" != "-" ]; then
        before="$(doctor_fixture_sha256 "$target/$artifact_rel")"
    fi
    local phase
    for phase in before after; do
        "$ee_bin" doctor --workspace "$target" "${doctor_args[@]}" > "$base/report-$phase.json"
        if ! jq -es --arg check "$check_name" --arg code "$error_code" \
            "length == 1 and (.[0] | .schema == \"ee.response.v2\" and .success == true and ($filter))" \
            "$base/report-$phase.json" >/dev/null; then
            printf 'fixture assert: %s: doctor (%s) did not report %s %s %s --fix; see %s\n' \
                "$fm_id" "$view" "$check_name" "$error_code" "$phase" "$base/report-$phase.json" >&2
            return 1
        fi
        if [ "$phase" = before ]; then
            "$ee_bin" doctor --workspace "$target" --fix --json > "$base/report-fix.json"
            if ! jq -es '
                length == 1 and (.[0] |
                    .schema == "ee.response.v2" and .success == true and
                    .data.actionCount == 0 and (.data.fixerResults | length) == 0)
            ' "$base/report-fix.json" >/dev/null; then
                printf 'fixture assert: %s is REPORT-ONLY but --fix acted; upgrade the spec and fixture (V6); see %s\n' \
                    "$fm_id" "$base/report-fix.json" >&2
                return 1
            fi
        fi
    done
    if [ -n "$before" ] && [ "$(doctor_fixture_sha256 "$target/$artifact_rel")" != "$before" ]; then
        printf 'fixture assert: %s artifact %s changed during a report-only --fix\n' \
            "$fm_id" "$artifact_rel" >&2
        return 1
    fi
    printf 'report-only fixture confirmed: %s (%s %s reported, --fix 0 actions, still reported)\n' \
        "$fm_id" "$check_name" "$error_code" >&2
}

# Some damage is not in the store's bytes: an environment variable (the
# fixture's .fixture_baseline/env.sh) or a live process (a lock holder). Such a
# fixture ships a condition.sh next to corrupt.sh. It applies the condition to
# $EE_DOCTOR_FIXTURE_TARGET, checks that the condition took, runs the command
# it was given, and exits with that command's status. It exits
# DOCTOR_FIXTURE_CONDITION_NOT_APPLIED instead when the condition could not be
# applied (bd-2oh15 ruling t2250 R2). A harness that runs doctor on a fixture
# target must run it through doctor_fixture_under_condition, and must count a
# CONDITION_NOT_APPLIED run as neither a pass nor a skip that is silently
# forgiven: a run that did not exercise the condition proves nothing.
DOCTOR_FIXTURE_CONDITION_NOT_APPLIED=97

# doctor_fixture_under_condition <fm_dir> <target> <command> [args...]
# Runs the command under the fixture's condition, or directly when the fixture
# has no condition.sh. Returns the command's status, or 97 (see above).
doctor_fixture_under_condition() {
    local fm_dir="${1:?fixture source dir required}"
    local target="${2:?target required}"
    shift 2
    if [ -f "$fm_dir/condition.sh" ]; then
        EE_DOCTOR_FIXTURE_TARGET="$target" bash "$fm_dir/condition.sh" "$@"
    else
        "$@"
    fi
}

# The harness bucket of a fixture, from its manifest label (bd-2oh15 strand 4,
# ruling c9954). Prints one of:
#   coverage  REPAIR, GUIDANCE-ONLY: must pass.
#   gap       NOT-DETECTED, PINNED-DEFECT: must still reproduce its pinned gap.
#   untested  UNCLASSIFIED, UNRESOLVED: marker-only; never a pass, never a failure.
#   out_of_scope  OUT-OF-SCOPE: not a doctor failure mode (manifest scopeReason);
#             never run, never a pass, pinned by exact id in the ratchet.
#   unknown:<label>  anything else, including a fixture missing from the manifest.
doctor_fixture_bucket() {
    local fm_id="${1:?fm id required}"
    local manifest="${2:?manifest path required}"
    local label
    label="$(jq -r --arg id "$fm_id" '[.fixtures[] | select(.id == $id) | .label] | first // ""' "$manifest")"
    case "$label" in
        REPAIR | GUIDANCE-ONLY) printf 'coverage\n' ;;
        NOT-DETECTED | PINNED-DEFECT) printf 'gap\n' ;;
        UNCLASSIFIED | UNRESOLVED) printf 'untested\n' ;;
        OUT-OF-SCOPE) printf 'out_of_scope\n' ;;
        *) printf 'unknown:%s\n' "$label" ;;
    esac
}

# The manifest label of a fixture ("" when absent).
doctor_fixture_label() {
    local fm_id="${1:?fm id required}"
    local manifest="${2:?manifest path required}"
    jq -r --arg id "$fm_id" '[.fixtures[] | select(.id == $id) | .label] | first // ""' "$manifest"
}

# THE shared UNTESTED ratchet pin (bd-2oh15 rulings c9954 and the follow-up):
# one pin for every sub-harness that counts fixtures. It is exact in both
# directions: more untested fixtures than the pin fails (a new fixture must
# arrive classified), and fewer also fails until the pin is lowered in the same
# commit that classified the fixture, so the pin can only move down.
DOCTOR_FIXTURE_PIN_UNCLASSIFIED=3
DOCTOR_FIXTURE_PIN_UNRESOLVED=1
# OUT-OF-SCOPE fixtures are pinned by EXACT id (bd-2oh15 ruling on c9985), so
# relabelling a fixture OUT-OF-SCOPE can never be used to satisfy the pins
# above. Sorted, space separated. Each carries a manifest scopeReason.
DOCTOR_FIXTURE_OUT_OF_SCOPE_IDS="fm-policy_safety-redaction-class-coverage-gap fm-policy_safety-trauma-guard-policy-denied-exit-7"

# Counts the UNCLASSIFIED and UNRESOLVED fixture directories under
# <fixtures_src>, prints the counts for <harness>, and fails unless both equal
# their pins. Also prints the OUT-OF-SCOPE set on its own line and fails
# unless it equals DOCTOR_FIXTURE_OUT_OF_SCOPE_IDS exactly.
doctor_fixture_untested_ratchet() {
    local harness="${1:?harness name required}"
    local src="${2:?fixtures source required}"
    local manifest="$src/manifest.json"
    local unclassified=0 unresolved=0 out_of_scope="" fm_dir fm_id label
    for fm_dir in "$src"/fm-*; do
        [ -d "$fm_dir" ] || continue
        fm_id="$(basename "$fm_dir")"
        label="$(doctor_fixture_label "$fm_id" "$manifest")"
        case "$label" in
            UNCLASSIFIED) unclassified=$((unclassified + 1)) ;;
            UNRESOLVED) unresolved=$((unresolved + 1)) ;;
            OUT-OF-SCOPE) out_of_scope="$out_of_scope $fm_id" ;;
        esac
    done
    out_of_scope="$(printf '%s\n' $out_of_scope | LC_ALL=C sort | tr '\n' ' ' | sed 's/ *$//')"
    printf '%s: %s UNCLASSIFIED (not tested, pin %s); %s UNRESOLVED (not tested, pin %s)\n' \
        "$harness" "$unclassified" "$DOCTOR_FIXTURE_PIN_UNCLASSIFIED" \
        "$unresolved" "$DOCTOR_FIXTURE_PIN_UNRESOLVED" >&2
    printf '%s: %s OUT-OF-SCOPE (not doctor failure modes; never run, never a pass): %s\n' \
        "$harness" "$(printf '%s\n' $out_of_scope | grep -c .)" "${out_of_scope:-none}" >&2
    if [ "$out_of_scope" != "$DOCTOR_FIXTURE_OUT_OF_SCOPE_IDS" ]; then
        printf '%s: OUT-OF-SCOPE set changed; it is pinned by exact id (expected: %s)\n' \
            "$harness" "$DOCTOR_FIXTURE_OUT_OF_SCOPE_IDS" >&2
        return 1
    fi
    if [ "$unclassified" -gt "$DOCTOR_FIXTURE_PIN_UNCLASSIFIED" ] ||
        [ "$unresolved" -gt "$DOCTOR_FIXTURE_PIN_UNRESOLVED" ]; then
        printf '%s: UNTESTED ratchet exceeded; classify the new fixture instead of raising the pin\n' \
            "$harness" >&2
        return 1
    fi
    if [ "$unclassified" -lt "$DOCTOR_FIXTURE_PIN_UNCLASSIFIED" ] ||
        [ "$unresolved" -lt "$DOCTOR_FIXTURE_PIN_UNRESOLVED" ]; then
        printf '%s: UNTESTED ratchet pin is stale; lower DOCTOR_FIXTURE_PIN_* in tests/doctor_fixtures/lib.sh to %s/%s\n' \
            "$harness" "$unclassified" "$unresolved" >&2
        return 1
    fi
}
