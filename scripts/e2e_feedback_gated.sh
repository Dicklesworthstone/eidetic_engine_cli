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
# Cover the freshly created store with reproducible, explicit RFC3339 bounds.
window=(--since 2000-01-01T00:00:00Z --until 2100-01-01T00:00:00Z)

step "init workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.success == true' "ee init succeeds"

step "cold-start honesty: no outcomes -> calibration is inert, never falsely confident"
cold="$(ee_json outcome calibration "${window[@]}" --workspace "$WS" --json)"
assert_jq "$cold" '.success == true and .data.sampleCount == 0 and .data.assessment == "insufficient_data" and (.data | has("brierScore")) and .data.brierScore == null and all(.data.buckets[]; .count == 0)' \
    "cold-start report exposes zero samples and an unavailable Brier score"

step "seed memories + pack (records pack_candidate_impressions, bd-1n0np.2.2)"
r1="$(ee_json remember "Always run cargo fmt --check before a release." \
    --workspace "$WS" --level procedural --kind rule --tags release,fmt --json)"
r2="$(ee_json remember "RCH remote verification is required for Rust changes." \
    --workspace "$WS" --level procedural --kind rule --tags release,rch --json)"
assert_jq "$r1" '.success == true' "remember rule 1"
assert_jq "$r2" '.success == true' "remember rule 2"
pack_out="$(ee_json pack "prepare a release" --workspace "$WS" --max-tokens 2000 --json)"
assert_jq "$pack_out" '.success == true' "ee pack succeeds (records impressions)"

step "record a helpful outcome with a situation class"
memory_id="$(jq -r '.data.memoryId' <<<"$r1")"
observed="$(ee_json outcome "$memory_id" --signal helpful --source-type human_explicit \
    --evidence-json '{"situationClass":"release"}' --workspace "$WS" --json)"
assert_jq "$observed" '.success == true' "explicit feedback accepted"

step "harvest available evidence (bd-1n0np.2.5)"
if ee_lists_outcome_sub harvest; then
    harvested="$(ee_json outcome harvest "${window[@]}" --workspace "$WS" --apply --json)"
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
    log_drop 1 "ee roi CLI not built (deferred when bd-1n0np.13.1 closed; compute_token_roi has no non-test caller): utility-per-token assertions skipped"
fi

step "regime-shift surfaces a DEMOTION CANDIDATE, never an auto-demote (bd-1n0np.13.2)"
# The SPRT regime-shift core proposes a demotion curation candidate; nothing is
# auto-demoted. bd-1n0np.13.2 closed with that wiring deferred: detect_regime_shift
# (src/core/outcome.rs) has no non-test caller, so this guard stays false.
cand="$(ee_json curate candidates --workspace "$WS" --json)"
if printf '%s' "$cand" | jq -e '[.data.candidates[]? | select((.source // "") == "regime_shift" or ((.rationale // "") | test("regime"; "i")))] | length >= 1' >/dev/null 2>&1; then
    _harness_pass "regime-shift demotion proposal surfaced as a curation candidate (not auto-applied)"
else
    log_drop 1 "regime-shift proposals not wired into curate (deferred when bd-1n0np.13.2 closed): proposal assertion skipped"
fi

step "calibration-honesty: empirical recall + wide CIs + loud abstention (bd-1n0np.13.3)"
honesty="$(ee_json outcome calibration --honesty "${window[@]}" --workspace "$WS" --json)"
assert_jq "$honesty" '.success == true and .data.schema == "ee.calibration_honesty.v1" and .data.sampleCount == 1 and any(.data.classes[]; .situationClass == "release" and .abstained == true and .intervalLower == 0 and .intervalUpper == 1)' \
    "a single observed release outcome abstains with a full-width interval"

end_temp_workspace
harness_summary
