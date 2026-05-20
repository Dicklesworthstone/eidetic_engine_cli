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
    find "$target" -type f -print | sort | shasum -a 256 > "$target/.fixture_baseline/before.sha256"
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
        "$ee_bin" doctor --workspace "$target" --fix --only "$fm_id" --json > "$target/.fixture_baseline/doctor-fix.json"
        "$ee_bin" doctor --workspace "$target" --only "$fm_id" --json > "$target/.fixture_baseline/doctor-after.json"
        "$ee_bin" doctor --workspace "$target" undo --last --json > "$target/.fixture_baseline/doctor-undo.json"
        find "$target" -type f -print | sort | shasum -a 256 > "$target/.fixture_baseline/after-undo.sha256"
        cmp "$target/.fixture_baseline/before.sha256" "$target/.fixture_baseline/after-undo.sha256"
    fi

    printf 'assert fixture ready: %s %s %s\n' "$fm_id" "$severity" "$subsystem" >&2
}
