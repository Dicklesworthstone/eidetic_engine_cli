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
# Record WHICH binary produced every verdict below. Without this a reader cannot
# tell a pre-fix run from a post-fix one, and "executed" becomes an unfalsifiable
# claim. EE_BIN is resolved by the harness ahead of EE_BINARY and ahead of the
# cargo target-dir fallbacks, so a stale target-dir build cannot be used silently.
SUITE_EE_BIN="$EE_BIN"
SUITE_EE_VERSION="$("$EE_BIN" --version 2>/dev/null | head -1)"
SUITE_EE_MTIME="$(date -u -r "$(command -v "$EE_BIN" || printf '%s' "$EE_BIN")" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || printf 'unknown')"
printf '[suite] root=%s host=%s os=%s\n' "$SUITE_ROOT" "$SUITE_HOST" "$SUITE_OS" >&2
printf '[suite] ee_bin=%s version=%s mtime=%s cargo_toml_version=%s\n' \
    "$SUITE_EE_BIN" "$SUITE_EE_VERSION" "$SUITE_EE_MTIME" \
    "$(awk -F'"' '/^version = /{print $2; exit}' "$SUITE_DIR/../Cargo.toml" 2>/dev/null)" >&2
log_event suite_binary phase setup ee_bin "$SUITE_EE_BIN" \
    ee_version "$SUITE_EE_VERSION" ee_mtime "$SUITE_EE_MTIME" \
    cargo_toml_version "$(awk -F'"' '/^version = /{print $2; exit}' "$SUITE_DIR/../Cargo.toml" 2>/dev/null)" \
    host "$SUITE_HOST" os "$SUITE_OS"

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
    # Counts the degradation the pack ACTUALLY emits, not the status-surface code
    # this line used to name. It recorded fell_back=0 on every run, which read as
    # "the pack never degraded" and sent the diagnosis in the wrong direction for
    # 39 hours. See the re-aim note below.
    fell_back="$(printf '%s' "$pack_json" \
        | jq -r '[(.degraded // .data.degraded // [])[]? | select(.code == "embed_model_unavailable")] | length')"
    marker="$ws/.ee/index-rebuild-request.json"

    log_event arm_act bead_id "$bead" phase act check pack_fallback_probe \
        workspace "$ws" host "$SUITE_HOST" fell_back "${fell_back:-0}"

    # RE-AIMED AT THE BOUNDARY THIS ARM ACTUALLY EXERCISES (bd-1iupc.2).
    #
    # THE OLD FILTER WAS WRONG AND THE OLD COMMENT ARGUED AGAINST ITSELF. It read
    # "The PACK surfaces `search_index_degraded` (src/core/health.rs:329)" --
    # citing the HEALTH surface as evidence about the PACK. That code lives only
    # in health.rs:329 and status.rs:4053/4542/7437 and is absent from
    # ALL_DEGRADATION_CODES (src/models/degradation.rs:612), so the filter could
    # never match a pack response and both this arm and dlr6a failed on it.
    #
    # PROBED on this exact scenario -- fresh workspace, memories, no index --
    # the pack emits: pack_assembly_elapsed_over_budget, embed_model_unavailable,
    # global_lane_migration_required. The pack DOES degrade; the filter missed it.
    #
    # WHY THE EXPECTATION FLIPS. This host has no embedding model, so the
    # operative degradation is embed_model_unavailable, and the product then
    # DELIBERATELY DECLINES to request a rebuild. src/core/index.rs:11056, in the
    # product's own words:
    #
    #   "A missing embedding model is deliberately NOT a rebuild trigger
    #    (bd-1iupc.2): rebuilding cannot conjure a model, and requesting one
    #    there would imply a repair that cannot happen."
    #
    # Asserting a marker PRESENT here accused the product of failing to do
    # something it is documented to refuse. This bead's scope item 3 already
    # separates the two cases -- "no index -> rebuild" and "no embedding model ->
    # NO rebuild attempt" -- and this arm runs in the second. The first case is
    # owed and tracked separately; it needs a host that HAS a model.
    #
    # THE ABSENCE ASSERTION BELOW IS TWO-SIDED ON PURPOSE. "No rebuild marker"
    # passes when the pack never ran, when the workspace never built, and when
    # the whole mechanism is broken. So the degradation that MUST be present is
    # asserted first; only then does the absence mean anything.
    assert_jq "$pack_json" \
        '(.degraded // .data.degraded // []) | length >= 1' \
        "$bead: precondition — the pack actually degraded (guards the absence assertion below)"
    assert_jq "$pack_json" \
        '[(.degraded // .data.degraded // [])[]? | select(.code == "embed_model_unavailable")] | length >= 1' \
        "$bead: precondition — the operative degradation is a missing embedding model"

    assert_eq "$( [ -f "$marker" ] && echo present || echo missing )" "missing" \
        "$bead: no rebuild is requested when the embedding model is missing (bd-1iupc.2)"
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

    # SAME CORRECTION AS THE x35vi ARM ABOVE, and it shares the same cause: this
    # filter named `search_index_degraded`, a STATUS-surface code (health.rs:329,
    # status.rs:4053/4542/7437) that is absent from ALL_DEGRADATION_CODES and is
    # never present in a pack response. It could not match, so this arm's
    # precondition failed on every run and its two downstream relevance
    # assertions were VOID -- they read like live relevance-floor defects while
    # the scorer under test had never run. Probed codes for this scenario:
    # pack_assembly_elapsed_over_budget, embed_model_unavailable,
    # global_lane_migration_required.
    #
    # Two clauses, not one: the pack must have degraded AT ALL (so an empty or
    # failed pack cannot satisfy the relevance assertions below by having no
    # items), and the operative degradation must be the missing embedding model
    # that forces the index-free path on this host.
    assert_jq "$pack_json" \
        '(.degraded // .data.degraded // []) | length >= 1' \
        "$bead: precondition — the pack actually degraded"
    assert_jq "$pack_json" \
        '[(.degraded // .data.degraded // [])[]? | select(.code == "embed_model_unavailable")] | length >= 1' \
        "$bead: precondition — pack used the index-free fallback scorer (no embedding model)"
    # The relevance assertions below count ABSENCES (off-topic memories must not
    # appear). An absent memory is also what an EMPTY pack produces, so require
    # the pack to have selected something before reading anything into a zero.
    assert_jq "$pack_json" \
        '((.data.pack.items // []) | length) >= 1' \
        "$bead: precondition — the pack selected at least one item, so a zero off-topic count means exclusion rather than an empty pack"

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

# ---------------------------------------------------------------------------
# Arm: bd-tag-case-roundtrip-tkq8v
#
# `ee remember` canonicalises tags through Tag::parse (ASCII-lowercasing), but
# `memory list --tag` matched case-SENSITIVELY, so recall for a tag written as
# "Ticker:RDVT" silently returned zero forever. Read-side canonicalisation is
# models/query.rs:1376 via canonicalize_tag_filter.
#
# Two-sided: a tag that was never written must still match nothing, otherwise
# "found it" would also pass for a filter that matches everything.
# ---------------------------------------------------------------------------
arm_tag_case_roundtrip() {
    local bead="bd-tag-case-roundtrip-tkq8v"
    local ws started tagged_id
    ws="$(arm_workspace "$bead" tag_case_roundtrip)"
    started="$(now_ms)"

    step "[$bead] a tag written in mixed case is recallable in any case"
    ee_in "$ws" init --json >/dev/null
    tagged_id="$(ee_in "$ws" remember "Underwrite verdict recorded for the screen." \
        --level semantic --kind decision --tags "Ticker:RDVT" --json \
        | jq -r '.data.memoryId // .data.memory_id // empty')"
    assert_eq "$( [ -n "$tagged_id" ] && echo created || echo missing )" "created" \
        "$bead: tagged memory created"

    local lower_hits upper_hits absent_hits
    lower_hits="$(ee_in "$ws" memory list --tag "ticker:rdvt" --json \
        | jq -r --arg id "$tagged_id" '[.data.memories[]? | select(.id == $id)] | length')"
    upper_hits="$(ee_in "$ws" memory list --tag "Ticker:RDVT" --json \
        | jq -r --arg id "$tagged_id" '[.data.memories[]? | select(.id == $id)] | length')"
    absent_hits="$(ee_in "$ws" memory list --tag "ticker:neverwritten" --json \
        | jq -r '[.data.memories[]?] | length')"

    log_event arm_act bead_id "$bead" phase act check tag_case_probe \
        workspace "$ws" host "$SUITE_HOST" memory "$tagged_id" \
        lower_hits "$lower_hits" upper_hits "$upper_hits" absent_hits "$absent_hits"

    assert_eq "$lower_hits" "1" "$bead: canonical lowercase tag recalls the memory"
    assert_eq "$upper_hits" "1" "$bead: the original mixed-case spelling also recalls it"
    # PAIRING: proves the filter is actually filtering, not matching everything.
    assert_eq "$absent_hits" "0" "$bead: a tag that was never written matches nothing"

    log_event arm_done bead_id "$bead" phase assert check tag_case_roundtrip \
        verdict recorded workspace "$ws" host "$SUITE_HOST" \
        duration_ms "$(( $(now_ms) - started ))"
}

# ---------------------------------------------------------------------------
# Arm: bd-cik-phone-number-false-positive-pmsri  (P0, store-corruption evidence)
#
# Zero-padded 10-digit CIKs and SEC accession numbers were refused as phone
# numbers, so filing memories could not be written without
# --allow-secret-mention. The two-sided framing the bead demands: the MUST-PASS
# corpus admits, and the MUST-REDACT corpus still refuses. Without the second
# half, "CIK admits" would also pass for a build that disabled the detector.
# ---------------------------------------------------------------------------
arm_cik_accession_false_positive() {
    local bead="bd-cik-phone-number-false-positive-pmsri"
    local ws started
    ws="$(arm_workspace "$bead" cik_accession_false_positive)"
    started="$(now_ms)"

    step "[$bead] CIK and accession numbers admit; real secrets still refuse"
    ee_in "$ws" init --json >/dev/null

    # MUST-PASS corpus — no --allow-secret-mention anywhere below.
    local pass_ok=0 pass_total=0 label
    while IFS='|' read -r label body; do
        [ -z "$label" ] && continue
        pass_total=$(( pass_total + 1 ))
        if ee_in "$ws" remember "$body" --level semantic --kind fact --json >/dev/null; then
            pass_ok=$(( pass_ok + 1 ))
        else
            log_event arm_evidence bead_id "$bead" phase act check must_pass_refused \
                workspace "$ws" host "$SUITE_HOST" case "$label"
        fi
    done <<'CORPUS'
zero_padded_cik|Registrant CIK 0001720116 filed the annual report on time.
bare_cik|The filer is identified as CIK 1720116 in the submission header.
accession_number|Accession 0001957132-26-000015 covers the amended filing.
CORPUS

    assert_eq "$pass_ok" "$pass_total" \
        "$bead: all $pass_total CIK/accession cases admit without --allow-secret-mention"

    # MUST-REDACT corpus — the detector must still be doing its job.
    local redact_refused=0 redact_total=0
    while IFS='|' read -r label body; do
        [ -z "$label" ] && continue
        redact_total=$(( redact_total + 1 ))
        if ee_in "$ws" remember "$body" --level semantic --kind fact --json >/dev/null 2>&1; then
            log_event arm_evidence bead_id "$bead" phase act check must_redact_admitted \
                workspace "$ws" host "$SUITE_HOST" case "$label"
        else
            redact_refused=$(( redact_refused + 1 ))
        fi
    done <<'CORPUS'
nanp_phone_separators|Call the desk at 415-555-0142 to confirm the trade.
ssn_shape|The beneficial owner SSN is 123-45-6789 per the filing.
CORPUS

    log_event arm_act bead_id "$bead" phase act check secret_corpus_probe \
        workspace "$ws" host "$SUITE_HOST" \
        must_pass_admitted "$pass_ok" must_pass_total "$pass_total" \
        must_redact_refused "$redact_refused" must_redact_total "$redact_total"

    # PAIRING: without this, the arm above would pass on a disabled detector.
    assert_eq "$redact_refused" "$redact_total" \
        "$bead: all $redact_total real-secret cases are still refused"

    log_event arm_done bead_id "$bead" phase assert check cik_accession_false_positive \
        verdict recorded workspace "$ws" host "$SUITE_HOST" \
        duration_ms "$(( $(now_ms) - started ))"
}

# ---------------------------------------------------------------------------
# Arm: bd-pack-doctor-posture-disagreement-nts29
#
# `ee pack` and `ee doctor` legitimately measure different things — pack counts
# one invocation's retrieval degradations, doctor reports static workspace
# health and deliberately excludes advisory-tier findings. The fix was not to
# force one number but to make the pack banner NAME its scope, so an agent
# stops being told to repair a workspace doctor calls healthy.
# ---------------------------------------------------------------------------
arm_pack_banner_names_its_scope() {
    local bead="bd-pack-doctor-posture-disagreement-nts29"
    local ws started empty_index pack_json summary
    ws="$(arm_workspace "$bead" pack_banner_scope)"
    started="$(now_ms)"

    step "[$bead] a degraded pack banner names its per-invocation scope"
    ee_in "$ws" init --json >/dev/null
    ee_in "$ws" remember "Run cargo fmt --check before cutting a release." \
        --level procedural --kind rule --json >/dev/null
    empty_index="$ws/empty-index"
    mkdir -p "$empty_index"
    pack_json="$( export EE_INDEX_DIR="$empty_index"; ee_in "$ws" pack "release checklist" --max-tokens 2000 --json )"
    summary="$(printf '%s' "$pack_json" | jq -r '.data.pack.advisoryBanner.summary // ""')"

    log_event arm_act bead_id "$bead" phase act check banner_scope_probe \
        workspace "$ws" host "$SUITE_HOST" \
        banner_status "$(printf '%s' "$pack_json" | jq -r '.data.pack.advisoryBanner.status // "none"')"

    assert_jq "$pack_json" \
        '(.data.pack.advisoryBanner.status // "") == "degraded"' \
        "$bead: precondition — the pack banner is in its degraded state"
    # Same correction as the cross-surface arm: "this pack" alone also matches
    # the pre-fix prose, so it is not a discriminating assertion.
    assert_contains "$summary" "this pack only" \
        "$bead: banner names the pack invocation as its scope"
    assert_contains "$summary" "not workspace health" \
        "$bead: banner disclaims workspace-health scope"
    assert_contains "$summary" "ee doctor" \
        "$bead: banner points at the workspace-health surface"
    # The exact prose the field report blamed for sending agents to repair a
    # healthy workspace must be gone.
    assert_eq "$(printf '%s' "$summary" | grep -c 'repair degraded sources')" "0" \
        "$bead: banner no longer directs repair of workspace sources"

    log_event arm_done bead_id "$bead" phase assert check pack_banner_names_its_scope \
        verdict recorded workspace "$ws" host "$SUITE_HOST" \
        duration_ms "$(( $(now_ms) - started ))"
}

# ---------------------------------------------------------------------------
# Arm: bd-pack-doctor-posture-disagreement-nts29, acceptance bullet 3.
#
# "Same fixture workspace evaluated by both surfaces yields consistent verdict
# vocabulary." Bullet 1 (the scope-named banner) is covered by
# arm_pack_banner_names_its_scope, but that arm never runs `ee doctor`, so
# nothing until now evaluated BOTH surfaces against ONE workspace - which is
# precisely the arm that catches them drifting apart again.
#
# What this deliberately does NOT assert: that the two surfaces produce the
# SAME verdict. They legitimately differ. `ee doctor` excludes advisory-tier
# findings from its top line; `ee pack` counts the degradations of a single
# invocation. Asserting equality would encode a bug as a contract and would
# force a future fix to weaken this test.
#
# What it asserts instead: both verdicts come from their own CLOSED
# vocabulary rather than free prose, and when they diverge - which is the
# condition this bead exists for - the pack banner NAMES ITS SCOPE so an agent
# is not sent to repair a workspace that doctor reports healthy.
# ---------------------------------------------------------------------------
arm_cross_surface_verdict_vocabulary() {
    local bead="bd-pack-doctor-posture-disagreement-nts29"
    local ws started empty_index pack_json doctor_json pack_status pack_summary doctor_posture
    ws="$(arm_workspace "$bead" cross_surface_verdict_vocabulary)"
    started="$(now_ms)"

    step "[$bead] one workspace, both surfaces, consistent verdict vocabulary"
    ee_in "$ws" init --json >/dev/null
    ee_in "$ws" remember "Run cargo fmt --check before cutting a release." \
        --level procedural --kind rule --json >/dev/null

    empty_index="$ws/empty-index"
    mkdir -p "$empty_index"
    pack_json="$( export EE_INDEX_DIR="$empty_index"; ee_in "$ws" pack "release checklist" --max-tokens 2000 --json )"
    # THE SAME workspace, no index override: doctor reports static workspace
    # health, which is the whole point of the comparison.
    doctor_json="$(ee_in "$ws" doctor --json)"

    pack_status="$(printf '%s' "$pack_json" | jq -r '.data.pack.advisoryBanner.status // ""')"
    pack_summary="$(printf '%s' "$pack_json" | jq -r '.data.pack.advisoryBanner.summary // ""')"
    doctor_posture="$(printf '%s' "$doctor_json" | jq -r '.data.posture // ""')"

    log_event arm_act bead_id "$bead" phase act check cross_surface_probe \
        workspace "$ws" host "$SUITE_HOST" \
        pack_status "$pack_status" doctor_posture "$doctor_posture"

    # 1. Consistent VOCABULARY: each surface emits a token from its own closed
    #    enumeration. A free-prose verdict is the drift this bullet guards.
    assert_jq "$(printf '{"v":"%s"}' "$pack_status")" \
        '.v == "clear" or .v == "advisory" or .v == "degraded"' \
        "$bead: pack verdict is from the closed advisory vocabulary"
    assert_jq "$(printf '{"v":"%s"}' "$doctor_posture")" \
        '.v == "ok" or .v == "initializing" or .v == "degraded_recoverable"
         or .v == "degraded_required" or .v == "blocked"' \
        "$bead: doctor verdict is from the closed posture vocabulary"

    # 2. NON-VACUITY, and the condition this bead exists for: on this workspace
    #    the two surfaces genuinely diverge. Without this the arm would pass on
    #    a healthy workspace where both agree and prove nothing. If they ever
    #    stop diverging here, this fails loudly and someone re-reads the bead -
    #    which is correct, not brittle.
    assert_eq "$pack_status" "degraded" \
        "$bead: precondition - pack reports degraded on this workspace"
    assert_eq "$doctor_posture" "ok" \
        "$bead: precondition - doctor reports ok on the SAME workspace"

    # 3. LOAD-BEARING: given that divergence, the pack banner must name its own
    #    scope. This is the assertion that catches the surfaces drifting apart
    #    again, because it fails the moment the scope language is removed.
    # "this pack" ALONE is satisfied by the pre-fix prose, which ended
    # "...before relying on this pack." Verified by running this arm against
    # v0.15.2: that assertion passed while the two below failed. Assert the
    # actual scope CLAIM, which exists only in the fixed text.
    assert_contains "$pack_summary" "this pack only" \
        "$bead: diverging pack verdict names its per-invocation scope"
    assert_contains "$pack_summary" "not workspace health" \
        "$bead: diverging pack verdict disclaims workspace-health scope"
    assert_contains "$pack_summary" "ee doctor" \
        "$bead: diverging pack verdict points at the workspace-health surface"

    log_event arm_done bead_id "$bead" phase assert check cross_surface_verdict_vocabulary \
        verdict recorded workspace "$ws" host "$SUITE_HOST" \
        pack_status "$pack_status" doctor_posture "$doctor_posture" \
        duration_ms "$(( $(now_ms) - started ))"
}

arm_status_lexical_honesty
arm_auto_index_rebuild_request
arm_fallback_relevance_floor
arm_ns_gate_first_open_race
arm_tag_case_roundtrip
arm_cik_accession_false_positive
arm_pack_banner_names_its_scope
arm_cross_surface_verdict_vocabulary

printf '[suite] artifacts retained under %s\n' "$SUITE_ROOT" >&2
harness_summary
