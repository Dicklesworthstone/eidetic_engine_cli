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

doctor_fixture_corrupt() {
    local fm_id="${1:?fm id required}"
    local severity="${2:?severity required}"
    local subsystem="${3:?subsystem required}"
    local target
    target="$(doctor_fixture_target)"
    local marker_dir
    marker_dir="$(doctor_fixture_marker_dir "$target")"
    mkdir -p "$marker_dir" "$target/.fixture_baseline"
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
        -print | sort | shasum -a 256 > "$target/.fixture_baseline/before.sha256"
    printf '{"schema":"ee.doctor_fixture_marker.v1","fmId":"%s","severity":"%s","subsystem":"%s","state":"corrupt"}\n' \
        "$fm_id" "$severity" "$subsystem" > "$marker_dir/$fm_id.json"
    printf 'corrupt fixture prepared: %s\n' "$fm_id" >&2
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
        # invoke --fix on its own and use --only on the read-only diagnose pass
        # afterwards to scope the after-state inspection to this FM's code.
        "$ee_bin" doctor --workspace "$target" --fix --json > "$target/.fixture_baseline/doctor-fix.json"
        "$ee_bin" doctor --workspace "$target" --only "$fm_id" --json > "$target/.fixture_baseline/doctor-after.json"
        local run_id
        run_id="$(jq -r '.runId // .data.runId // empty' "$target/.fixture_baseline/doctor-fix.json")"
        if [ -z "$run_id" ]; then
            printf 'fixture assert: could not extract runId from fix output\n' >&2
            cat "$target/.fixture_baseline/doctor-fix.json" >&2
            return 1
        fi
        "$ee_bin" doctor --workspace "$target" --undo "$run_id" --json > "$target/.fixture_baseline/doctor-undo.json"
        find "$target" -type f \
            -not -path '*/.doctor/*' \
            -not -path '*/.fixture_baseline/*' \
            -not -path '*/.ee/doctor-fixtures/*' \
            -not -name '.assert.stdout' \
            -not -name '.assert.stderr' \
            -not -name '._*' \
            -print | sort | shasum -a 256 > "$target/.fixture_baseline/after-undo.sha256"
        cmp "$target/.fixture_baseline/before.sha256" "$target/.fixture_baseline/after-undo.sha256"
    fi

    printf 'assert fixture ready: %s %s %s\n' "$fm_id" "$severity" "$subsystem" >&2
}
