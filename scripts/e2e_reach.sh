#!/usr/bin/env bash
# bd-2vq2z.22 - Reach observability e2e.
#
# Scenario: run a real prebuilt ee binary against an isolated workspace; prove
# global-tier reach, config-explain truth-in-labeling, timeline as-of audit,
# scorecard trend, and task-specific gap explanation surfaces with structured
# logging. Missing not-yet-landed sibling CLI routes are visible log_drop rows,
# never silent passes.
#
# NOTE: no `set -e` - assert_* helpers accumulate failures and harness_summary
# decides the exit code, so a single failing assertion cannot prevent artifacts
# and the summary from being written.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Avoid the shared harness's cargo-metadata fallback in code-first swarm lanes.
EE_BIN="${EE_BIN:-ee}"
export EE_BIN

# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "reach"

ee_json() {
    e2e_log_command "$EE_BIN" "$@" || true
}

ee_has_config_explain() {
    "$EE_BIN" config --help 2>&1 | grep -qE '(^|[[:space:]])explain([[:space:]]|$)'
}

ee_has_global_tier_cli() {
    "$EE_BIN" remember --help 2>&1 | grep -q -- "--global" \
        && "$EE_BIN" pack --help 2>&1 | grep -q -- "--no-global"
}

ee_has_timeline_cli() {
    "$EE_BIN" timeline --help >/dev/null 2>&1
}

health_scorecard_mode() {
    if "$EE_BIN" health scorecard --help >/dev/null 2>&1; then
        printf '%s\n' "subcommand"
    elif "$EE_BIN" health --scorecard --help >/dev/null 2>&1; then
        printf '%s\n' "flag"
    else
        printf '%s\n' "none"
    fi
}

run_health_scorecard() {
    local mode="$1"
    shift
    case "$mode" in
        subcommand) ee_json health scorecard "$@" ;;
        flag) ee_json health --scorecard "$@" ;;
        *) return 1 ;;
    esac
}

gap_report_mode() {
    if "$EE_BIN" why-not --help 2>&1 | grep -q -- "--gaps"; then
        printf '%s\n' "why-not"
    elif "$EE_BIN" pack --help 2>&1 | grep -q -- "--explain-gaps"; then
        printf '%s\n' "pack"
    else
        printf '%s\n' "none"
    fi
}

run_gap_report() {
    local mode="$1"
    local task="$2"
    local workspace="$3"
    case "$mode" in
        why-not) ee_json why-not --task "$task" --gaps --workspace "$workspace" --json ;;
        pack) ee_json pack "$task" --workspace "$workspace" --max-tokens 360 --read-only --explain-gaps --json ;;
        *) return 1 ;;
    esac
}

json_number_or_empty() {
    local json="$1"
    local filter="$2"
    printf '%s' "$json" | jq -r "$filter" 2>/dev/null | head -n 1
}

gap_missing_kind_count() {
    local json="$1"
    printf '%s' "$json" | jq -r '
        [.. | objects | select(has("missingKinds")) | .missingKinds[]?] | length
    ' 2>/dev/null || printf '%s\n' "0"
}

first_missing_kind() {
    local json="$1"
    local kind
    kind="$(printf '%s' "$json" | jq -r '
        first(.. | objects | select(has("missingKinds")) | .missingKinds[]?) // "rule"
    ' 2>/dev/null | tr '[:upper:]' '[:lower:]' | tr -cd 'a-z0-9_-')"
    if [ -n "$kind" ]; then
        printf '%s\n' "$kind"
    else
        printf '%s\n' "rule"
    fi
}

assert_number_lt() {
    local actual="$1"
    local expected_upper="$2"
    local label="$3"
    local result
    result="$(
        python3 - "$actual" "$expected_upper" <<'PY'
import sys
try:
    actual = float(sys.argv[1])
    upper = float(sys.argv[2])
except Exception:
    print("false")
else:
    print("true" if actual < upper else "false")
PY
    )"
    e2e_log_assert_eq "$result" "true" "$label"
    if [ "$result" = "true" ]; then
        _harness_pass "$label ($actual < $expected_upper)"
    else
        _harness_fail "$label: expected $actual < $expected_upper"
    fi
}

with_temp_workspace WS

step "init isolated workspace"
init_out="$(ee_json init --workspace "$WS" --json)"
assert_jq "$init_out" '.schema == "ee.response.v2" and .success == true' \
    "ee init returns a success response envelope"

step "config explain reports semantic-weight truth-in-labeling"
if ee_has_config_explain; then
    explain_out="$(ee_json config explain search.semantic_weight --workspace "$WS" --json)"
    assert_jq "$explain_out" '.schema == "ee.response.v2" and .success == true' \
        "config explain returns a success response envelope"
    assert_jq "$explain_out" '.data.schema == "ee.config_explain.v1"' \
        "config explain emits the stable v1 data schema"
    assert_jq "$explain_out" '.data.key == "search.semantic_weight"' \
        "config explain identifies the requested key"
    assert_jq "$explain_out" '.data.status == "active"' \
        "semantic weight is active, not mislabeled as unwired"
    assert_jq "$explain_out" '
        ((.data.caveat // "") | contains("vector"))
        and ((.data.caveat // "") | contains("neural"))
    ' "semantic weight caveat explains vector-vs-neural truth"
    assert_jq "$explain_out" '
        (.data.effectiveValue == null or (.data.effectiveValue | type) == "string")
        and (
            .data.sourceLayer == "cli"
            or .data.sourceLayer == "environment"
            or .data.sourceLayer == "project"
            or .data.sourceLayer == "user"
            or .data.sourceLayer == "default"
            or .data.sourceLayer == "unknown"
        )
        and ((.data.lintFindings // null) | type) == "array"
    ' "config explain carries effective value, source layer, and lint findings"
else
    log_drop 1 "ee config explain surface pending for bd-2vq2z.15: when wired, assert ee.config_explain.v1 for search.semantic_weight is active and caveats vector-vs-neural semantics"
fi

step "doctor config-lint remains advisory-only when surfaced"
doctor_out="$(
    EMBEDDING_MODEL="/tmp/ee-e2e-missing-model2vec.bin" \
        ee_json doctor --workspace "$WS" --json
)"
assert_jq "$doctor_out" '.schema == "ee.response.v2" and .success == true' \
    "ee doctor returns a success response envelope"
log_event "reach_doctor_contract_assertion" \
    assertion "doctor envelope exposes degraded array for advisory config-lint context" \
    bead "bd-2vq2z.15"
assert_jq "$doctor_out" '(.degraded // null) | type == "array"' \
    "doctor envelope exposes a machine-readable degraded array"
if printf '%s' "$doctor_out" | jq -e '
    [.. | objects | select(((.code? // "") | tostring | startswith("config_")))]
    | length >= 1
' >/dev/null 2>&1; then
    assert_jq "$doctor_out" '
        all([.. | objects | select(((.code? // "") | tostring | startswith("config_")))][]; (.severity? // "advisory") == "advisory")
    ' "doctor config-lint findings are advisory-only"
    assert_jq "$doctor_out" '
        ((.data.posture // .data.overall.posture // .data.health.posture // "") | tostring) != "blocked"
    ' "doctor top-line posture is not blocked by advisory config-lint"
else
    log_drop 1 "doctor config-lint block pending for bd-2vq2z.15: when wired, assert config_* findings are advisory and never flip top-line doctor health"
fi

step "user-global memory tier is local, provenance-tagged, and opt-out capable"
if ee_has_global_tier_cli; then
    WS_A="$WS/reach-global-a"
    WS_B="$WS/reach-global-b"
    USER_DATA="$WS/user-data"
    USER_HOME="$WS/home"
    WS_A_DB="$WS_A/.ee/ee.db"
    WS_A_INDEX="$WS_A/.ee/index"
    WS_B_DB="$WS_B/.ee/ee.db"
    WS_B_INDEX="$WS_B/.ee/index"
    GLOBAL_DB="$USER_DATA/ee/global/ee.db"
    mkdir -p "$WS_A/.ee" "$WS_B/.ee" "$USER_DATA" "$USER_HOME"

    log_event "global_memory_e2e_setup" \
        bead "bd-2vq2z.13" \
        userData "$USER_DATA" \
        globalDb "$GLOBAL_DB" \
        workspaceA "$WS_A" \
        workspaceB "$WS_B"

    init_a="$(
        HOME="$USER_HOME" XDG_DATA_HOME="$USER_DATA" \
            EE_DATABASE_PATH="$WS_A_DB" EE_INDEX_DIR="$WS_A_INDEX" \
            ee_json init --workspace "$WS_A" --json
    )"
    init_b="$(
        HOME="$USER_HOME" XDG_DATA_HOME="$USER_DATA" \
            EE_DATABASE_PATH="$WS_B_DB" EE_INDEX_DIR="$WS_B_INDEX" \
            ee_json init --workspace "$WS_B" --json
    )"
    assert_jq "$init_a" '.schema == "ee.response.v2" and .success == true' \
        "global tier workspace A init succeeds"
    assert_jq "$init_b" '.schema == "ee.response.v2" and .success == true' \
        "global tier workspace B init succeeds"

    global_rule="Global rule: before cross-repo release work, run remote RCH verification only."
    workspace_conflict="Workspace rule: before cross-repo release work, do not run local cargo; wait for central verify."

    remember_global="$(
        HOME="$USER_HOME" XDG_DATA_HOME="$USER_DATA" \
            EE_DATABASE_PATH="$WS_A_DB" EE_INDEX_DIR="$WS_A_INDEX" \
            ee_json remember "$global_rule" \
            --workspace "$WS_A" \
            --global \
            --level procedural \
            --kind rule \
            --source "test://bd-2vq2z.13/global-rule" \
            --json
    )"
    assert_jq "$remember_global" '.schema == "ee.response.v2" and .success == true' \
        "remember --global stores a global rule"
    assert_jq "$remember_global" '
        ((.data.globalMemory.schema // .data.global.schema // .data.memory.global.schema // "") == "ee.global_memory.v1")
        or ((.data.provenance.lane // .data.memory.provenanceLane // .data.lane // "") == "global")
    ' "remember --global reports global store metadata or global provenance lane"
    assert_jq "$remember_global" "
        ([.. | scalars | tostring | select(contains(\"$GLOBAL_DB\"))] | length) >= 1
        or ([.. | objects | select(((.databasePath? // \"\") | contains(\"/global/ee.db\")))] | length) >= 1
    " "remember --global reports the separate user-global database path"

    remember_conflict="$(
        HOME="$USER_HOME" XDG_DATA_HOME="$USER_DATA" \
            EE_DATABASE_PATH="$WS_B_DB" EE_INDEX_DIR="$WS_B_INDEX" \
            ee_json remember "$workspace_conflict" \
            --workspace "$WS_B" \
            --level procedural \
            --kind rule \
            --source "test://bd-2vq2z.13/workspace-conflict" \
            --json
    )"
    assert_jq "$remember_conflict" '.schema == "ee.response.v2" and .success == true' \
        "workspace conflicting rule stores locally"

    pack_default="$(
        HOME="$USER_HOME" XDG_DATA_HOME="$USER_DATA" \
            EE_DATABASE_PATH="$WS_B_DB" EE_INDEX_DIR="$WS_B_INDEX" \
            ee_json pack "prepare cross-repo release verification" \
            --workspace "$WS_B" \
            --max-tokens 800 \
            --read-only \
            --json
    )"
    assert_jq "$pack_default" '.schema == "ee.response.v2" and .success == true' \
        "pack includes global tier by default"
    assert_jq "$pack_default" '
        ([.. | objects | select(((.schema? // "") == "ee.global_memory.v1"))] | length) >= 1
        or ([.. | objects | select(((.lane? // .provenanceLane? // .memoryLane? // "") == "global"))] | length) >= 1
    ' "default pack exposes global store metadata or global provenance lane"
    assert_jq "$pack_default" "
        ([.. | scalars | tostring | select(contains(\"$GLOBAL_DB\"))] | length) >= 1
        or ([.. | objects | select(((.databasePath? // \"\") | contains(\"/global/ee.db\")))] | length) >= 1
    " "default pack identifies the separate user-global store"
    assert_jq "$pack_default" '
        ([.. | objects | select(((.uri? // "") == "test://bd-2vq2z.13/global-rule"))] | length) >= 1
        or ((.data.pack.text // "") | contains("remote RCH verification"))
    ' "default pack includes the remembered global rule with provenance"
    assert_jq "$pack_default" '
        ([.. | objects | select(((.kind? // .conflictKind? // .globalConflictKind? // "") == "contradiction")
            or ((.conflictKey? // "") | test("release|verification")))] | length) >= 1
    ' "conflicting workspace/global rules are surfaced, not silently resolved"

    pack_no_global="$(
        HOME="$USER_HOME" XDG_DATA_HOME="$USER_DATA" \
            EE_DATABASE_PATH="$WS_B_DB" EE_INDEX_DIR="$WS_B_INDEX" \
            ee_json pack "prepare cross-repo release verification" \
            --workspace "$WS_B" \
            --no-global \
            --max-tokens 800 \
            --read-only \
            --json
    )"
    assert_jq "$pack_no_global" '.schema == "ee.response.v2" and .success == true' \
        "--no-global pack still succeeds"
    assert_jq "$pack_no_global" '
        ([.. | objects | select(((.lane? // .provenanceLane? // .memoryLane? // "") == "global"))] | length) == 0
        and ((.data.pack.text // "") | contains("remote RCH verification") | not)
    ' "--no-global excludes global-tier memories"
else
    log_drop 1 "bd-2vq2z.13 global-tier CLI flags absent: when wired, assert remember --global writes the separate ~/.local/share/ee/global store, default pack includes global provenance/conflicts, and --no-global excludes it"
fi

step "health scorecard declines after duplicate and stale-provenance fixture"
SCORECARD_MODE="$(health_scorecard_mode)"
if [ "$SCORECARD_MODE" != "none" ]; then
    SCORE_WS="$WS/reach-scorecard"
    SCORE_EVIDENCE="$SCORE_WS/evidence.md"
    mkdir -p "$SCORE_WS"
    printf '%s\n' "Reach scorecard evidence stays stable." > "$SCORE_EVIDENCE"

    score_init="$(ee_json init --workspace "$SCORE_WS" --json)"
    assert_jq "$score_init" '.schema == "ee.response.v2" and .success == true' \
        "scorecard workspace init succeeds"

    score_rule="$(
        ee_json remember "Scorecard coverage rule: use structured reach evidence before memory maintenance." \
            --workspace "$SCORE_WS" \
            --level procedural \
            --kind rule \
            --tags reach,scorecard,coverage \
            --source "file://$SCORE_EVIDENCE:1" \
            --json
    )"
    score_decision="$(
        ee_json remember "Decision: reach scorecard trend snapshots compare before and after store degradation." \
            --workspace "$SCORE_WS" \
            --level procedural \
            --kind decision \
            --tags reach,scorecard,trend \
            --source "file://$SCORE_EVIDENCE:1" \
            --json
    )"
    assert_jq "$score_rule" '.schema == "ee.response.v2" and .success == true' \
        "scorecard baseline rule memory stores"
    assert_jq "$score_decision" '.schema == "ee.response.v2" and .success == true' \
        "scorecard baseline decision memory stores"

    score_before="$(run_health_scorecard "$SCORECARD_MODE" --workspace "$SCORE_WS" --json)"
    log_event "scorecard_before_snapshot" \
        bead "bd-2vq2z.14" \
        mode "$SCORECARD_MODE"
    assert_jq "$score_before" '.schema == "ee.response.v2" and .success == true' \
        "health scorecard before snapshot succeeds"
    assert_jq "$score_before" '
        (.data.schema == "ee.health_scorecard.v1")
        and ((.data.subScores // []) | type == "array")
        and ((.data.topActions // []) | type == "array")
    ' "health scorecard exposes schema, subScores, and topActions"

    for idx in 1 2 3; do
        dup_out="$(
            ee_json remember "Scorecard duplicate rule: repeated reach-observability advice should be deduplicated." \
                --workspace "$SCORE_WS" \
                --level procedural \
                --kind rule \
                --tags reach,scorecard,duplicate \
                --source "file://$SCORE_EVIDENCE:1" \
                --json
        )"
        assert_jq "$dup_out" '.schema == "ee.response.v2" and .success == true' \
            "scorecard duplicate memory $idx stores"
    done
    stale_out="$(
        ee_json remember "Scorecard stale provenance: this memory references a missing reach evidence file." \
            --workspace "$SCORE_WS" \
            --level semantic \
            --kind fact \
            --tags reach,scorecard,stale \
            --source "file://$SCORE_WS/missing-evidence.md:1" \
            --json
    )"
    assert_jq "$stale_out" '.schema == "ee.response.v2" and .success == true' \
        "scorecard stale-provenance memory stores"

    score_after="$(run_health_scorecard "$SCORECARD_MODE" --workspace "$SCORE_WS" --json)"
    log_event "scorecard_after_snapshot" \
        bead "bd-2vq2z.14" \
        mode "$SCORECARD_MODE"
    assert_jq "$score_after" '.schema == "ee.response.v2" and .success == true' \
        "health scorecard after degradation succeeds"
    before_score="$(json_number_or_empty "$score_before" '(.data.compositeScore // .data.score.overall // .data.score // empty)')"
    after_score="$(json_number_or_empty "$score_after" '(.data.compositeScore // .data.score.overall // .data.score // empty)')"
    if [ -n "$before_score" ] && [ -n "$after_score" ]; then
        assert_number_lt "$after_score" "$before_score" \
            "health scorecard declines after duplicate and stale-provenance fixture"
    else
        log_drop 1 "bd-2vq2z.14 scorecard score field not numeric yet: when wired, assert composite score declines between before/after snapshots"
    fi
    assert_jq "$score_after" '
        ([.data.topActions[]? | (.. | scalars | tostring)] | join(" ") | test("duplicate|redund|drift|provenance|freshness"; "i"))
    ' "health scorecard top actions name redundancy or provenance repair"
    assert_jq "$score_after" '
        ((.data.trend.direction // "") | test("declin|worsen|down"; "i"))
        or ((.data.trend.delta // .data.trend.scoreDelta // 0) < 0)
    ' "health scorecard trend reports decline"
else
    log_drop 1 "bd-2vq2z.14 health scorecard route absent: when wired, assert ee.health_scorecard.v1 score decline, trend direction, and duplicate/provenance top actions"
fi

step "timeline reconstructs as-of state with fixture timestamps"
if ee_has_timeline_cli; then
    old_policy="$(
        ee_json remember "Reach timeline audit policy: use RCH before release." \
            --workspace "$WS" \
            --level procedural \
            --kind rule \
            --tags reach,timeline,audit \
            --source "test://bd-2vq2z.22/timeline-old-policy" \
            --valid-from "2026-05-01T00:00:00Z" \
            --valid-to "2026-05-03T00:00:00Z" \
            --json
    )"
    new_policy="$(
        ee_json remember "Reach timeline audit policy: central batch verify owns release proof." \
            --workspace "$WS" \
            --level procedural \
            --kind rule \
            --tags reach,timeline,audit \
            --source "test://bd-2vq2z.22/timeline-new-policy" \
            --valid-from "2026-05-03T00:00:00Z" \
            --json
    )"
    decision="$(
        ee_json remember "Decision: reach timeline audit uses memory validity windows." \
            --workspace "$WS" \
            --level procedural \
            --kind decision \
            --tags reach,timeline,decision \
            --source "test://bd-2vq2z.22/timeline-decision" \
            --valid-from "2026-05-02T00:00:00Z" \
            --json
    )"
    assert_jq "$old_policy" '.schema == "ee.response.v2" and .success == true' \
        "timeline old policy stores with fixed validity"
    assert_jq "$new_policy" '.schema == "ee.response.v2" and .success == true' \
        "timeline new policy stores with fixed validity"
    assert_jq "$decision" '.schema == "ee.response.v2" and .success == true' \
        "timeline decision stores with fixed validity"
    OLD_ID="$(printf '%s' "$old_policy" | jq -r '.data.memoryId // .data.memory_id // .data.id')"
    NEW_ID="$(printf '%s' "$new_policy" | jq -r '.data.memoryId // .data.memory_id // .data.id')"
    DECISION_ID="$(printf '%s' "$decision" | jq -r '.data.memoryId // .data.memory_id // .data.id')"
    log_event "reach_timeline_fixture_seeded" \
        bead "bd-2vq2z.16" \
        oldMemory "$OLD_ID" \
        newMemory "$NEW_ID" \
        decisionMemory "$DECISION_ID" \
        asOf "2026-05-02T12:00:00Z"

    timeline_out="$(
        ee_json timeline "reach timeline audit" \
            --workspace "$WS" \
            --as-of "2026-05-02T12:00:00Z" \
            --limit 20 \
            --json
    )"
    assert_jq "$timeline_out" '.schema == "ee.response.v2" and .success == true' \
        "timeline returns a success response envelope"
    assert_jq "$timeline_out" '.data.schema == "ee.timeline.v1" and .data.command == "timeline"' \
        "timeline emits ee.timeline.v1 data"
    assert_jq "$timeline_out" '.data.topic == "reach timeline audit" and .data.asOf == "2026-05-02T12:00:00Z"' \
        "timeline echoes topic and normalized as-of timestamp"
    assert_jq "$timeline_out" \
        "([.data.memoriesThen[].memoryId] | index(\"$OLD_ID\") != null)
            and ([.data.decisionsInEffect[].memoryId] | index(\"$DECISION_ID\") != null)" \
        "timeline includes memories and decisions in effect at as-of"
    assert_jq "$timeline_out" \
        "([.data.changesSince[] | select(.memoryId == \"$NEW_ID\" and .changeType == \"added\")] | length) == 1
            and ([.data.changesSince[] | select(.memoryId == \"$OLD_ID\" and .changeType == \"superseded\")] | length) == 1" \
        "timeline reports added and superseded changes since as-of"
else
    log_drop 1 "bd-2vq2z.16 timeline route absent: when wired, assert ee.timeline.v1 memoriesThen, decisionsInEffect, and changesSince over fixture timestamps"
fi

step "task-specific why-not gaps produce capture demand and improve after capture"
GAP_MODE="$(gap_report_mode)"
if [ "$GAP_MODE" != "none" ]; then
    GAP_TASK="stabilize reach query gap for quartz release rehearsal"
    gap_before="$(run_gap_report "$GAP_MODE" "$GAP_TASK" "$WS")"
    log_event "gap_report_before_capture" \
        bead "bd-2vq2z.17" \
        mode "$GAP_MODE" \
        task "$GAP_TASK"
    assert_jq "$gap_before" '.schema == "ee.response.v2" and .success == true' \
        "why-not/pack gap report succeeds before capture"
    assert_jq "$gap_before" '
        ([.. | objects | select(.schema? == "ee.coverage_gap.v1")] | length) >= 1
        or (.data.coverageGap.schema == "ee.coverage_gap.v1")
        or (.data.gaps.schema == "ee.coverage_gap.v1")
    ' "gap report exposes ee.coverage_gap.v1"
    assert_jq "$gap_before" '
        ([.. | objects | select(has("missingKinds")) | .missingKinds[]?] | length) >= 1
    ' "gap report names missing memory kinds"
    assert_jq "$gap_before" '
        ([.. | objects | select(has("captureTemplates")) | .captureTemplates[]? | (.command? // .template? // "") | tostring | select(startswith("ee remember "))] | length) >= 1
    ' "gap report offers remember capture templates"

    before_missing_count="$(gap_missing_kind_count "$gap_before")"
    capture_kind="$(first_missing_kind "$gap_before")"
    captured_gap="$(
        ee_json remember "Reach gap capture: $GAP_TASK now has an explicit $capture_kind memory with provenance." \
            --workspace "$WS" \
            --level procedural \
            --kind "$capture_kind" \
            --tags reach,gaps,captured \
            --source "test://bd-2vq2z.17/captured-gap" \
            --json
    )"
    assert_jq "$captured_gap" '.schema == "ee.response.v2" and .success == true' \
        "gap capture memory stores"

    gap_after="$(run_gap_report "$GAP_MODE" "$GAP_TASK" "$WS")"
    log_event "gap_report_after_capture" \
        bead "bd-2vq2z.17" \
        mode "$GAP_MODE" \
        task "$GAP_TASK" \
        capturedKind "$capture_kind"
    assert_jq "$gap_after" '.schema == "ee.response.v2" and .success == true' \
        "why-not/pack gap report succeeds after capture"
    after_missing_count="$(gap_missing_kind_count "$gap_after")"
    assert_number_lt "$after_missing_count" "$before_missing_count" \
        "capturing a suggested memory reduces missing-kind count"
else
    log_drop 1 "bd-2vq2z.17 why-not --gaps / pack --explain-gaps route absent: when wired, assert ee.coverage_gap.v1 missingKinds, captureTemplates, and reduced gaps after capture"
fi

end_temp_workspace
harness_summary
