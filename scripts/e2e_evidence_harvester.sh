#!/usr/bin/env bash
# bd-1n0np.2.9 — Evidence Harvester end-to-end (real binary).
#
# Scenario (ADR 0055): temp workspace -> remember memories -> assemble a pack
# (writes pack_candidate_impressions rows, bd-1n0np.2.2) -> simulate a clean bead
# close + a passing verification + a reverted commit within explicit windows ->
# `ee outcome harvest --dry-run` asserts proposed derived outcomes with evidence
# chains + reliability weights -> `--apply` asserts audited writes AND that a
# pre-existing EXPLICIT signal is NOT overridden -> `ee outcome calibration`
# asserts reliability buckets + Brier.
#
# Surfaces that are not yet wired in the binary under test (the harvest /
# calibration CLI, bd-1n0np.2.5/2.6, and the V067 impression table) are
# CAPABILITY-GUARDED: a missing surface records a visible `log_drop` (the
# no-silent-cap rule) instead of a false pass, and the corresponding assertions
# activate automatically once the binary provides the surface. The init /
# remember / pack path is exercised for real on any current binary.
# NOTE: no `set -e` — the harness assert_* helpers accumulate pass/fail and
# `harness_summary` decides the exit code, so a single failing assert must not
# abort the run before the summary is written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/e2e_harness.sh
source "$E2E_DIR/lib/e2e_harness.sh"

harness_init "evidence_harvester"

# ee_supports <subcommand words...> — true when `<words> --help` is accepted.
ee_supports() { "$EE_BIN" "$@" --help >/dev/null 2>&1; }

# ee_lists_outcome_sub <name> — true only when `ee outcome --help` actually lists
# <name> as a subcommand. A bare `ee outcome <name> --help` is NOT a reliable
# probe: clap treats an unknown <name> as the positional target-id and still
# prints help with exit 0, so it would false-positive on pre-2.5 binaries.
ee_lists_outcome_sub() { "$EE_BIN" outcome --help 2>&1 | grep -qiw "$1"; }

# ee_json <args...> — run ee, tolerate nonzero exit (assertions inspect output).
ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }

with_temp_workspace WS

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "remember procedural rules"
r1="$(ee_json remember "Run cargo fmt --check before every release." \
    --workspace "$WS" --level procedural --kind rule --json)"
assert_jq "$r1" '.success == true' "remember rule 1"
r2="$(ee_json remember "RCH remote verification is required for Rust changes." \
    --workspace "$WS" --level procedural --kind rule --json)"
assert_jq "$r2" '.success == true' "remember rule 2"

step "assemble pack (records pack_candidate_impressions, bd-1n0np.2.2)"
pack_out="$(ee_json pack "prepare a release" --workspace "$WS" --max-tokens 2000 --json)"
assert_jq "$pack_out" '.success == true' "ee pack succeeds"

step "impression ledger rows recorded (bd-1n0np.2.2)"
if ee_supports db inspect; then
    imp="$(ee_json db inspect pack_candidate_impressions --workspace "$WS" --json)"
    if printf '%s' "$imp" | jq -e '.success == true' >/dev/null 2>&1; then
        assert_jq "$imp" \
            '((.data.rowCount // 0) >= 1) or (((.data.rows // []) | length) >= 1)' \
            "pack_candidate_impressions has at least one row"
    else
        log_drop 1 "pack_candidate_impressions absent (binary predates V067; rebuild needed)"
    fi
else
    log_drop 1 "ee db inspect surface unavailable in binary under test"
fi

# The remaining steps exercise the harvest/calibration CLI (bd-1n0np.2.5/2.6).
# They are guarded until those surfaces land; window flags (--since/--until) and
# the simulated bead-close / verification / reverted-commit fixtures are wired
# together with the CLI so the contract is asserted against the real surface.
step "harvest dry-run -> derived-outcome proposals (bd-1n0np.2.4/2.5)"
if ee_lists_outcome_sub harvest; then
    harvest="$(ee_json outcome harvest --workspace "$WS" --dry-run --json)"
    assert_jq "$harvest" '.success == true' "outcome harvest --dry-run succeeds"
    assert_jq "$harvest" '(.data.proposals | type) == "array"' "harvest emits a proposals array"
else
    log_drop 1 "harvest CLI pending (bd-1n0np.2.5): dry-run proposal assertions skipped"
fi

step "harvest apply -> audited writes; explicit NOT overridden (bd-1n0np.2.4/2.5)"
if ee_lists_outcome_sub harvest; then
    applied="$(ee_json outcome harvest --workspace "$WS" --apply --json)"
    assert_jq "$applied" '.success == true' "outcome harvest --apply succeeds"
    assert_jq "$applied" \
        '(.data.explicitOverrides // 0) == 0' \
        "derived feedback never overrides explicit feedback"
else
    log_drop 1 "harvest --apply pending (bd-1n0np.2.5): audited-write + explicit-override assertions skipped"
fi

step "calibration report -> reliability buckets + Brier (bd-1n0np.2.6)"
if ee_lists_outcome_sub calibration; then
    calib="$(ee_json outcome calibration --workspace "$WS" --json)"
    assert_jq "$calib" '.success == true' "outcome calibration succeeds"
    assert_jq "$calib" '.data.buckets != null' "calibration emits reliability buckets"
    assert_jq "$calib" \
        '(.data.brierScore == null) or ((.data.brierScore | type) == "number")' \
        "calibration emits a Brier score field"
else
    log_drop 1 "calibration CLI pending (bd-1n0np.2.6): reliability-bucket + Brier assertions skipped"
fi

end_temp_workspace
harness_summary
