#!/usr/bin/env bash
# bd-2vq2z.15 - Config reachability / truth-in-labeling e2e.
#
# Scenario: run a real prebuilt ee binary against an isolated workspace; prove
# the config-explain surface returns stable JSON for the semantic-weight trap
# once the CLI route is wired, and keep doctor config-lint visibility explicit
# with structured no-silent-cap logging while that wiring lands.
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

end_temp_workspace
harness_summary
