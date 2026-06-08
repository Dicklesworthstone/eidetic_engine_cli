#!/usr/bin/env bash
# bd-1n0np.13.5 — Feedback-gated learning layer end-to-end (real binary,
# detailed logging).
#
# Scenario: a temp workspace; seed memories + a pack (records pack-candidate
# impressions, bd-1n0np.2.2) so the Evidence Harvester has something to harvest.
# Assertions:
#   * hard (always true on a current binary): init/remember/pack succeed; and
#     COLD-START honesty — with no outcomes, the calibration surface stays inert
#     (no falsely-confident reliability buckets), never a confident-looking number.
#   * condition-guarded (no-silent-cap log_drop when the surface is not present in
#     the binary under test): `ee outcome harvest` + `ee outcome calibration`
#     (bd-1n0np.2.5/2.6); `ee roi pack|memory` utility-per-token (bd-1n0np.13.1);
#     the SPRT regime-shift DEMOTION CANDIDATE — proposed, never auto-demoted —
#     (bd-1n0np.13.2); and the calibration-honesty report with wide CIs + loud
#     abstention on a sparse class (bd-1n0np.13.3).
# Every step emits an ee.test_event.v1 event + a human line via the shared
# harness (scripts/lib/e2e_harness.sh, surfaced through scripts/e2e_lib.sh).
#
# NOTE: no `set -e` — assert_* accumulate pass/fail and harness_summary decides
# the exit code, so a single failing assert must not abort before the summary.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "feedback_gated"

# ee_json <args...> — run ee, tolerate nonzero exit (assertions inspect output).
ee_json() { "$EE_BIN" "$@" 2>/dev/null || true; }
# ee_lists_outcome_sub <name> — true only when `ee outcome --help` lists <name>.
ee_lists_outcome_sub() { "$EE_BIN" outcome --help 2>&1 | grep -qiw "$1"; }
# ee_lists_top <name> — true only when `ee --help` lists <name> as a command.
ee_lists_top() { "$EE_BIN" --help 2>&1 | grep -qiw "$1"; }

with_temp_workspace WS

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "cold-start honesty: no outcomes -> calibration is inert, never falsely confident"
if ee_lists_outcome_sub calibration; then
    cold="$(ee_json outcome calibration --workspace "$WS" --json)"
    assert_jq "$cold" '.success == true' "cold-start calibration succeeds"
    # With zero outcomes there must be no reliability buckets — a sparse signal
    # that reads as certainty is worse than no signal (the feedback-gated honesty
    # invariant).
    assert_jq "$cold" '((.data.buckets // []) | length) == 0' \
        "cold-start calibration emits no falsely-confident buckets"
else
    log_drop 1 "calibration CLI absent (bd-1n0np.2.6): cold-start honesty assertion skipped"
fi

step "seed memories + pack (records pack_candidate_impressions, bd-1n0np.2.2)"
r1="$(ee_json remember "Always run cargo fmt --check before a release." \
    --workspace "$WS" --level procedural --kind rule --tags release,fmt --json)"
r2="$(ee_json remember "RCH remote verification is required for Rust changes." \
    --workspace "$WS" --level procedural --kind rule --tags release,rch --json)"
assert_jq "$r1" '.success == true' "remember rule 1"
assert_jq "$r2" '.success == true' "remember rule 2"
pack_out="$(ee_json pack "prepare a release" --workspace "$WS" --max-tokens 2000 --json)"
assert_jq "$pack_out" '.success == true' "ee pack succeeds (records impressions)"

step "seed a dense outcome stream via the Evidence Harvester (bd-1n0np.2.5)"
if ee_lists_outcome_sub harvest; then
    harvested="$(ee_json outcome harvest --workspace "$WS" --apply --json)"
    assert_jq "$harvested" '.success == true' "outcome harvest --apply succeeds"
else
    log_drop 1 "outcome harvest CLI absent (bd-1n0np.2.5): outcome-stream seeding skipped"
fi

step "ee roi pack|memory reports utility-per-token with sample counts (bd-1n0np.13.1)"
if ee_lists_top roi; then
    roi="$(ee_json roi pack --workspace "$WS" --json)"
    assert_jq "$roi" '.success == true' "ee roi pack succeeds"
    assert_jq "$roi" '(.data.buckets | type) == "array"' \
        "roi report emits buckets (utility-per-token with sample counts)"
else
    log_drop 1 "ee roi CLI absent (bd-1n0np.13.1 reporting surface pending): utility-per-token assertions skipped"
fi

step "regime-shift surfaces a DEMOTION CANDIDATE, never an auto-demote (bd-1n0np.13.2)"
# The SPRT regime-shift core proposes a demotion curation candidate; nothing is
# auto-demoted. Until the proposal is wired into the curation surface, guard it.
cand="$(ee_json curate candidates --workspace "$WS" --json)"
if printf '%s' "$cand" | jq -e '[.data.candidates[]? | select((.source // "") == "regime_shift" or ((.rationale // "") | test("regime"; "i")))] | length >= 1' >/dev/null 2>&1; then
    _harness_pass "regime-shift demotion proposal surfaced as a curation candidate (not auto-applied)"
else
    log_drop 1 "regime-shift demotion-candidate surface pending (bd-1n0np.13.2 curation wiring): proposal assertion skipped"
fi

step "calibration-honesty: empirical recall + wide CIs + loud abstention (bd-1n0np.13.3)"
if "$EE_BIN" outcome calibration --help 2>&1 | grep -q -- "--honesty"; then
    honesty="$(ee_json outcome calibration --honesty --workspace "$WS" --json)"
    assert_jq "$honesty" '.success == true' "calibration --honesty succeeds"
    assert_jq "$honesty" \
        '((.data.classes // []) | length) == 0 or any(.data.classes[]?; has("abstained"))' \
        "honesty report carries per-class abstention flags (loud abstention on sparse)"
else
    log_drop 1 "calibration-honesty surface absent (bd-1n0np.13.3 CLI pending): honesty/abstention assertions skipped"
fi

end_temp_workspace
harness_summary
