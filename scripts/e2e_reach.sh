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

end_temp_workspace
harness_summary
