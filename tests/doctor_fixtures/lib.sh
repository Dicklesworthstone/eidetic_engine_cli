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
    # Everything else, including ee.db, ee.db-wal and .doctor.lock, stays
    # byte-compared.
    find "$target" -type f \
        -not -path '*/.doctor/*' \
        -not -path '*/.fixture_baseline/*' \
        -not -path '*/.ee/doctor-fixtures/*' \
        -not -path '*/.ee/ee.db-shm' \
        -not -path '*/.ee/ee.write.lock' \
        -not -name '.assert.stdout' \
        -not -name '.assert.stderr' \
        -not -name '._*' \
        -exec shasum -a 256 -- {} + | LC_ALL=C sort | shasum -a 256
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
    doctor_fixture_record_write_lock "$target"
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
# repair for these writes state that cannot pass through
# doctor_runtime::mutate() or be undone (for example, an index rebuild writes
# SQLite rows), so doctor records guidance instead of repairing. The contract is
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
    "$ee_bin" doctor --workspace "$target" --fix --json > "$target/.fixture_baseline/doctor-fix.json"
    if ! jq -es --arg code "$finding_code" '
        length == 1 and (.[0] |
            .schema == "ee.response.v2" and .success == true and
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
        if ! cmp "$target/.fixture_baseline/before.sha256" "$target/.fixture_baseline/after-undo.sha256"; then
            printf 'fixture assert: undo contents differ from the baseline for %s\n' "$fm_id" >&2
            return 1
        fi
        doctor_fixture_assert_write_lock_monotonic "$fm_id" "$target"
    fi

    printf 'assert fixture ready: %s %s %s\n' "$fm_id" "$severity" "$subsystem" >&2
}
