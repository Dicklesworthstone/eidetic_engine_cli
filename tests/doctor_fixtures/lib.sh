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
    find "$target" -type f \
        -not -path '*/.doctor/*' \
        -not -path '*/.fixture_baseline/*' \
        -not -path '*/.ee/doctor-fixtures/*' \
        -not -name '.assert.stdout' \
        -not -name '.assert.stderr' \
        -not -name '._*' \
        -exec shasum -a 256 -- {} + | LC_ALL=C sort | shasum -a 256
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
    fi

    printf 'assert fixture ready: %s %s %s\n' "$fm_id" "$severity" "$subsystem" >&2
}
