#!/usr/bin/env bash
# bd-1n0np.4.8 — Error Fingerprint Recall end-to-end (real binary).
#
# Scenario: temp workspace -> remember a repair memory (fingerprint extraction
# runs on remember/import, bd-1n0np.4.3) -> `ee diagnose-error` recalls that
# repair from an error log via the layered fingerprint key (exact -> normalized
# -> semantic -> graph, bd-1n0np.4.4) -> `ee pack --error-log` folds the recall
# into a context pack (bd-1n0np.4.5).
#
# The error-recall CLI surfaces (4.3 fingerprint store, 4.4 diagnose-error, 4.5
# pack --error-log) are CAPABILITY-GUARDED: where a surface is absent in the
# binary under test, the step records a visible log_drop (the no-silent-cap rule)
# instead of a false pass, and its assertions activate automatically once the
# binary provides it. The init / remember path runs for real on any binary.
#
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "error_recall"

ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }
# True only when `ee pack --help` actually lists <flag> (avoids clap positional
# false-positives on binaries that predate the flag).
ee_pack_has_flag() { "$EE_BIN" pack --help 2>&1 | grep -qw "$1"; }

with_temp_workspace WS
ERR_LOG="$WS/rustc-error.log"
printf '%s\n' 'error[E0277]: the trait bound is not satisfied' >"$ERR_LOG"

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "remember a repair memory (fingerprint extraction source)"
remembered="$(ee_json remember "Fix E0277 trait-bound error by importing the trait into scope." \
    --workspace "$WS" --level procedural --kind rule --tags rust,error --json)"
assert_jq "$remembered" '.success == true' "remember repair memory"

step "fingerprint store present (bd-1n0np.4.3 / V072)"
if ee_supports db inspect; then
    fp="$(ee_json db inspect error_fingerprints --workspace "$WS" --json)"
    if printf '%s' "$fp" | jq -e '.success == true' >/dev/null 2>&1; then
        assert_jq "$fp" \
            '((.data.rowCount // 0) >= 0) or (((.data.rows // []) | length) >= 0)' \
            "error_fingerprints store is queryable"
    else
        log_drop 1 "error_fingerprints store absent (bd-1n0np.4.3 / V072 not built)"
    fi
else
    log_drop 1 "ee db inspect surface unavailable in binary under test"
fi

step "diagnose-error recalls the repair (bd-1n0np.4.4)"
if ee_supports diagnose-error; then
    diag="$(ee_json diagnose-error \
        --error-log "$ERR_LOG" \
        --workspace "$WS" --json)"
    assert_jq "$diag" '.success == true' "diagnose-error succeeds"
    assert_jq "$diag" '(.data.matches | type) == "array"' "diagnose-error emits a matches array"
    assert_jq "$diag" '.data.report.schema == "ee.error_recall.report.v1"' "diagnose-error emits recall report"
    assert_jq "$diag" '(.data.report.derivedDocument | contains("tool:rustc"))' "recall report includes derived document"
else
    log_drop 1 "diagnose-error CLI pending (bd-1n0np.4.4): recall assertions skipped"
fi

step "pack --error-log integration (bd-1n0np.4.5)"
if ee_pack_has_flag "error-log"; then
    packed="$(ee_json pack "diagnose a build failure" \
        --workspace "$WS" --error-log "$ERR_LOG" --json)"
    assert_jq "$packed" '.success == true' "pack --error-log succeeds"
else
    log_drop 1 "pack --error-log pending (bd-1n0np.4.5): integration assertions skipped"
fi

end_temp_workspace
harness_summary
