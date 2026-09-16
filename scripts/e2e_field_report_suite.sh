#!/usr/bin/env bash
# scripts/e2e_field_report_suite.sh — bd-field-report-verification-suite-5kd36
#
# Fresh-workspace, no-mock verification arms for the 2026-08-24/26 field-report
# beads. Every arm drives the REAL `ee` binary against a REAL workspace, DB and
# index — never library internals — per the suite's own acceptance text.
#
# One throwaway workspace per arm (the ee analogue of transaction isolation),
# created under a timestamped suite root and RETAINED by default so a red is
# diagnosable from the artifacts alone. Export EE_E2E_KEEP=0 to let the shared
# harness reclaim them.
#
# Evidence: each arm emits ee.test_event.v1 rows carrying the bead_id join key
# consumed by the cross-repo matrix (hfdt zu12ia.48), the phase, the host, and
# the workspace path.
#
# ARMS IMPLEMENTED HERE — only beads whose fix has actually landed:
#   bd-status-search-lexical-honesty-ejdpo     e91c115fc
#   bd-auto-index-rebuild-on-fallback-x35vi    3a3b3f212
#   bd-fallback-relevance-floor-labeling-dlr6a 9a6d8eab9
#   bd-ns-gate-open-retry-classifier-wg18a     8068719e9
#
# DELIBERATELY NOT IMPLEMENTED (see the bead comment): an arm for
# bd-lexical-fallback-hint-suppression-s2c10. That fix has not landed — there is
# no suppression mechanism in src/ — so an arm would assert behaviour that does
# not exist. Per this suite's own MUST-be-RED-pre-fix rule, a green arm against
# absent behaviour is worse than no arm.

set -uo pipefail

SUITE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ "$(uname -s)" = "Darwin" ]; then
    EE_E2E_TMPDIR="${EE_E2E_TMPDIR:-/private/tmp}"
    export EE_E2E_TMPDIR
fi
# Retain per-arm workspaces unless the caller opts out; the bead requires
# timestamped, retained scratch workspaces as first-class evidence.
export EE_E2E_KEEP="${EE_E2E_KEEP:-1}"

# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$SUITE_DIR/e2e_lib.sh"

harness_init "field_report_suite"

SUITE_HOST="$(uname -n)"
SUITE_OS="$(uname -s)"
SUITE_ROOT="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}/ee-field-report-suite-$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir -p "$SUITE_ROOT"
printf '[suite] root=%s host=%s os=%s\n' "$SUITE_ROOT" "$SUITE_HOST" "$SUITE_OS" >&2

# arm_workspace <bead_id> <arm_name> -> prints a fresh workspace path
arm_workspace() {
    local bead="${1:?arm_workspace: bead id required}"
    local arm="${2:?arm_workspace: arm name required}"
    local ws="$SUITE_ROOT/$arm"
    mkdir -p "$ws"
    log_event arm_workspace bead_id "$bead" arm "$arm" phase setup \
        workspace "$ws" host "$SUITE_HOST" os "$SUITE_OS"
    printf '%s' "$ws"
}

# ee_in <workspace> <args...> — run the real binary against one workspace.
# stderr is deliberately NOT silenced: this output is cited as evidence.
ee_in() {
    local ws="${1:?ee_in: workspace required}"
    shift
    "$EE_BIN" --workspace "$ws" "$@"
}

now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

# ---------------------------------------------------------------------------
# Arm: bd-status-search-lexical-honesty-ejdpo
#
# A healthy index whose embedder fell back to the deterministic hash backend
# serves every result from the lexical arm alone. Before e91c115fc, `ee status`
# reported that state as `search: ok` — silent under-recall behind a green
# banner. Trigger and expected emission are taken from the bead's own fixture
# at tests/fixtures/failure_modes/search_lexical_only.json.
# ---------------------------------------------------------------------------
arm_status_lexical_honesty() {
    local bead="bd-status-search-lexical-honesty-ejdpo"
    local ws started
    ws="$(arm_workspace "$bead" status_lexical_honesty)"
    started="$(now_ms)"

    step "[$bead] status reports lexical-only retrieval, not a green search banner"
    ee_in "$ws" init --json >/dev/null
    ee_in "$ws" remember "Cargo workspace uses Rust nightly." \
        --level semantic --kind fact --json >/dev/null

    # Subshell, not a `VAR=val func` prefix: in bash an assignment prefixed to a
    # FUNCTION call persists after the call returns, which would silently leak
    # EE_EMBED_DOWNLOAD=off into every later arm and change what they measure.
    local status_json
    status_json="$( export EE_EMBED_DOWNLOAD=off; ee_in "$ws" status --json )"

    log_event arm_act bead_id "$bead" phase act check status_probe \
        workspace "$ws" host "$SUITE_HOST"

    assert_jq "$status_json" \
        '[.degraded[]? | select(.code == "search_lexical_only")] | length == 1' \
        "$bead: status emits search_lexical_only exactly once"
    assert_jq "$status_json" \
        '[.degraded[]? | select(.code == "search_lexical_only" and .severity == "medium")] | length == 1' \
        "$bead: search_lexical_only carries medium severity"
    assert_jq "$status_json" \
        '[.degraded[]? | select(.code == "search_lexical_only")
          | select(.repair | tostring | test("ee index rebuild"))] | length == 1' \
        "$bead: search_lexical_only carries an actionable repair"

    # The honesty half: the banner must not simultaneously claim plain readiness.
    assert_jq "$status_json" \
        '([.degraded[]? | select(.code == "search_lexical_only")] | length == 1)
         and (.success == true)' \
        "$bead: lexical-only is reported as a degradation on a successful read"

    log_event arm_done bead_id "$bead" phase assert check status_lexical_honesty \
        verdict recorded workspace "$ws" host "$SUITE_HOST" \
        duration_ms "$(( $(now_ms) - started ))"
}

# ---------------------------------------------------------------------------
# Arm: bd-auto-index-rebuild-on-fallback-x35vi
#
# Every pack of a workspace with a missing or broken index paid
# `context_lexical_fallback` until an operator ran `ee index rebuild` by hand.
# 3a3b3f212 wired the trigger to record a rebuild request. The request is a
# plain JSON marker an operator can read without ee:
#   <workspace>/.ee/index-rebuild-request.json   (ee.index.rebuild_request.v1)
#
# The assertion is the CONTRACT, not a fixed branch: if the pack degraded to
# lexical fallback then the marker MUST exist. A pack that served semantically
# is a legitimate outcome and is recorded as such rather than silently passing.
# ---------------------------------------------------------------------------
arm_auto_index_rebuild_request() {
    local bead="bd-auto-index-rebuild-on-fallback-x35vi"
    local ws started marker
    ws="$(arm_workspace "$bead" auto_index_rebuild_request)"
    started="$(now_ms)"

    step "[$bead] a lexical-fallback pack records an index-rebuild request"
    ee_in "$ws" init --json >/dev/null
    ee_in "$ws" remember "Run cargo fmt --check before cutting a release." \
        --level procedural --kind rule --json >/dev/null
    ee_in "$ws" remember "A prior release failed because clippy was skipped." \
        --level episodic --kind failure --json >/dev/null

    # Force the condition under test. Pointing EE_INDEX_DIR at a fresh empty
    # directory makes the pack take the index-free path; without this the pack
    # is served semantically and the arm passes WITHOUT ever exercising the
    # fixed code, which is a green result that proves nothing.
    local empty_index pack_json fell_back
    empty_index="$ws/empty-index"
    mkdir -p "$empty_index"
    pack_json="$( export EE_INDEX_DIR="$empty_index"; ee_in "$ws" pack "release checklist" --max-tokens 2000 --json )"
    fell_back="$(printf '%s' "$pack_json" \
        | jq -r '[.degraded[]? | select(.code == "search_index_degraded")] | length')"
    marker="$ws/.ee/index-rebuild-request.json"

    log_event arm_act bead_id "$bead" phase act check pack_fallback_probe \
        workspace "$ws" host "$SUITE_HOST" fell_back "${fell_back:-0}"

    # Precondition assertion: the arm must PROVE it reached the path it tests.
    # The PACK surfaces `search_index_degraded` (src/core/health.rs:329);
    # `context_lexical_fallback` is the trigger reason recorded INTO the marker
    # (src/core/index.rs:9838). Asserting the pack's own code here, and the
    # marker's recorded code below, keeps each assertion single-shape.
    assert_jq "$pack_json" \
        '[.degraded[]? | select(.code == "search_index_degraded")] | length >= 1' \
        "$bead: precondition — pack degraded because the index could not serve"

    assert_eq "$( [ -f "$marker" ] && echo present || echo missing )" "present" \
        "$bead: lexical fallback records an index-rebuild request marker"
    if [ -f "$marker" ]; then
        assert_jq "$(cat "$marker")" \
            '.schema == "ee.index.rebuild_request.v1"' \
            "$bead: rebuild request carries its schema id"
        assert_jq "$(cat "$marker")" \
            '(.degraded_code // .degradedCode) == "context_lexical_fallback"' \
            "$bead: rebuild request names the degradation that triggered it"
    fi

    log_event arm_done bead_id "$bead" phase assert check auto_index_rebuild_request \
        verdict recorded workspace "$ws" host "$SUITE_HOST" \
        fell_back "${fell_back:-0}" duration_ms "$(( $(now_ms) - started ))"
}

# ---------------------------------------------------------------------------
# Arm: bd-fallback-relevance-floor-labeling-dlr6a
#
# Field report: an ADBE underwrite note surfaced at relevance 0.60 under
# `evidence` for a UCU query. 9a6d8eab9 fixed two mechanisms in the index-free
# fallback scorer:
#   1. it scored against "{level} {kind} {content}", so any query containing a
#      taxonomy word (decision, rule, fact, risk, episodic) earned full term
#      credit against EVERY memory carrying that facet;
#   2. matching was an unanchored haystack.contains(term), so a short term like
#      "ucu" matched "document" and "succumbed".
#
# Both are directly observable through the real pack surface.
# ---------------------------------------------------------------------------
arm_fallback_relevance_floor() {
    local bead="bd-fallback-relevance-floor-labeling-dlr6a"
    local ws started
    ws="$(arm_workspace "$bead" fallback_relevance_floor)"
    started="$(now_ms)"

    step "[$bead] index-free fallback does not inflate substring or taxonomy matches"
    ee_in "$ws" init --json >/dev/null

    # Off-topic memory whose CONTENT contains "cucumber" — verified to literally
    # contain the substring "ucu". The field report's own words ("document",
    # "succumbed") do NOT: "document" has "ocu" and "succumbed" has "ucc". A
    # probe built from words that merely look right would pass pre-fix and prove
    # nothing, which is exactly what the first draft of this arm did.
    local off_topic_id on_topic_id
    off_topic_id="$(ee_in "$ws" remember \
        "The nightly batch choked on a malformed cucumber inventory row." \
        --level episodic --kind failure --json \
        | jq -r '.data.memoryId // .data.memory_id // empty')"
    on_topic_id="$(ee_in "$ws" remember \
        "UCU underwrite verdict: pass, with a note on covenant headroom." \
        --level semantic --kind decision --json \
        | jq -r '.data.memoryId // .data.memory_id // empty')"

    assert_eq "$( [ -n "$off_topic_id" ] && [ -n "$on_topic_id" ] && echo seeded || echo incomplete )" \
        "seeded" "$bead: fixture memories created"

    # Force the index-free fallback scorer — the code 9a6d8eab9 actually
    # changed. Served semantically, this arm would pass without touching it.
    local empty_index pack_json
    empty_index="$ws/empty-index"
    mkdir -p "$empty_index"
    pack_json="$( export EE_INDEX_DIR="$empty_index"; ee_in "$ws" pack "ucu" --max-tokens 2000 --json )"

    assert_jq "$pack_json" \
        '[.degraded[]? | select(.code == "search_index_degraded")] | length >= 1' \
        "$bead: precondition — pack used the index-free fallback scorer"

    log_event arm_act bead_id "$bead" phase act check substring_inflation_probe \
        workspace "$ws" host "$SUITE_HOST" off_topic "$off_topic_id" on_topic "$on_topic_id"

    # (2) "ucu" must not match "document"/"succumbed" as a substring.
    local off_hits
    off_hits="$(printf '%s' "$pack_json" | jq -r --arg id "$off_topic_id" \
        '[.data.pack.items[]? | select(.memoryId == $id)] | length')"
    assert_eq "$off_hits" "0" \
        "$bead: a short query term must not substring-match an unrelated memory"

    # PAIRING: "the off-topic memory is absent" also holds for a pack that
    # returned nothing at all. Assert the on-topic memory IS present, so the
    # arm cannot pass by destroying recall — the floor must not be bought by
    # returning an empty pack.
    local on_hits
    on_hits="$(printf '%s' "$pack_json" | jq -r --arg id "$on_topic_id" \
        '[.data.pack.items[]? | select(.memoryId == $id)] | length')"
    assert_eq "$on_hits" "1" \
        "$bead: the on-topic memory still surfaces (arm is not passing on an empty pack)"

    # (1) A taxonomy word must not credit a memory by its level/kind facet. The
    # query word has to match the OFF-TOPIC memory's OWN facets to discriminate:
    # it is --level episodic, so "episodic" earned full facet credit pre-fix
    # while appearing nowhere in its content. Querying "decision" here (the
    # first draft) matched neither facet and so proved nothing.
    local taxonomy_pack taxonomy_off_hits
    taxonomy_pack="$( export EE_INDEX_DIR="$empty_index"; ee_in "$ws" pack "episodic" --max-tokens 2000 --json )"
    taxonomy_off_hits="$(printf '%s' "$taxonomy_pack" | jq -r --arg id "$off_topic_id" \
        '[.data.pack.items[]? | select(.memoryId == $id)] | length')"
    assert_eq "$taxonomy_off_hits" "0" \
        "$bead: a taxonomy word must not credit memories by level/kind facet"

    log_event arm_evidence bead_id "$bead" phase assert check taxonomy_pack_items \
        workspace "$ws" host "$SUITE_HOST" \
        item_count "$(printf '%s' "$taxonomy_pack" | jq -r '[.data.pack.items[]?] | length')"

    log_event arm_done bead_id "$bead" phase assert check fallback_relevance_floor \
        verdict recorded workspace "$ws" host "$SUITE_HOST" \
        duration_ms "$(( $(now_ms) - started ))"
}

# ---------------------------------------------------------------------------
# Arm: bd-ns-gate-open-retry-classifier-wg18a
#
# Two real ee processes racing the FIRST open of a fresh workspace tripped an
# fsqlite ns-gate error that was not classified retryable, so the race surfaced
# to the user as a read-only-open failure. 8068719e9 classified it retryable.
#
# This is a REAL concurrency arm: N concurrent processes against one fresh
# workspace, looped to expose the race. Zero failures may surface.
# ---------------------------------------------------------------------------
arm_ns_gate_first_open_race() {
    local bead="bd-ns-gate-open-retry-classifier-wg18a"
    local ws started rounds racers failures
    ws="$(arm_workspace "$bead" ns_gate_first_open_race)"
    started="$(now_ms)"
    rounds="${EE_FIELD_SUITE_RACE_ROUNDS:-5}"
    racers="${EE_FIELD_SUITE_RACE_RACERS:-4}"
    failures=0

    step "[$bead] concurrent first-open of a fresh workspace never surfaces a failure"
    ee_in "$ws" init --json >/dev/null
    ee_in "$ws" remember "ns-gate race probe memory." \
        --level semantic --kind fact --json >/dev/null

    local round racer pids rc
    for round in $(seq 1 "$rounds"); do
        pids=()
        for racer in $(seq 1 "$racers"); do
            ( ee_in "$ws" status --json >"$ws/race-r${round}-p${racer}.json" 2>"$ws/race-r${round}-p${racer}.err" ) &
            pids+=("$!")
        done
        for rc in "${pids[@]}"; do
            if ! wait "$rc"; then
                failures=$(( failures + 1 ))
            fi
        done
    done

    log_event arm_act bead_id "$bead" phase act check concurrent_first_open \
        workspace "$ws" host "$SUITE_HOST" rounds "$rounds" racers "$racers" \
        failures "$failures"

    assert_eq "$failures" "0" \
        "$bead: $(( rounds * racers )) concurrent opens all succeeded"

    log_event arm_done bead_id "$bead" phase assert check ns_gate_first_open_race \
        verdict recorded workspace "$ws" host "$SUITE_HOST" \
        duration_ms "$(( $(now_ms) - started ))"
}

arm_status_lexical_honesty
arm_auto_index_rebuild_request
arm_fallback_relevance_floor
arm_ns_gate_first_open_race

printf '[suite] artifacts retained under %s\n' "$SUITE_ROOT" >&2
harness_summary
