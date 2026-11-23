#!/usr/bin/env bash
# bd-reality-core-convergence-1azkt.7.1 -- does behaviour fixed in source reach
# the binary users actually download?
#
# WHY THIS EXISTS. The 2026-09-22 reality check (.ntm/swarm/REALITY_CHECK_2026-
# 09-22.md, proposal N1) found by hand what no gate checked: the published
# v0.15.2 archive reproduces a CLOSED P0 (bd-3h6bz, a validated rule that
# search finds but pack omits), admits every memory for an unrelated query, and
# reports a degraded status posture that doctor contradicts -- while the same
# binary passes the release-mode walking skeleton. Source-level tests cannot
# see this, because the defect is release lag: the fixes exist, the shipped
# bytes do not have them.
#
# WHAT IT DOES. Takes one release archive (URL or path), refuses to execute it
# unless its SHA-256 matches the published .sha256 (or --sha256), unpacks it,
# and drives the real binary in an isolated HOME/XDG tree with downloads off.
# Every response is captured to disk, then graded against PRE-REGISTERED
# expectations. The grader reads only the captured files, which is what lets
# --self-test prove each failure direction without a binary or a network.
#
# THE NEURAL PRECONDITION. The relevance and determinism findings were measured
# in the default neural configuration. A binary that silently fell back to hash
# embeddings would answer a different question, so `neural_backend_active` is a
# PRECONDITION: when it is false the verdict is `incomplete` (exit 3), never
# `pass`. Supply the Model2Vec cache with --model-cache <models-dir>; the
# default is $HOME/.local/share/ee/models when it holds the model. The cache is
# CLONED into the isolated tree (APFS clone / reflink when available), never
# linked, so the probe cannot mutate the operator's cache.
#
# Usage:
#   scripts/release_binary_probe.sh --archive <url|path> [--sha256 <hex>]
#       [--expected-target <triple>] [--output <file>]
#       [--model-cache <models-dir>] [--workdir <dir>] [--json]
#   scripts/release_binary_probe.sh --replay <capture-dir>
#       [--expected-target <triple>] [--output <file>] [--json]
#   scripts/release_binary_probe.sh --verify-release-dir <dir>
#       [--allow-compile-only <triple>]... [--proof-capsule <file>]
#       [--candidate-sha <commit> --candidate-version <version>]
#   scripts/release_binary_probe.sh --self-test
#
# Output (--json): one `ee.release_probe.v1` object on stdout, keyed by the
# archive SHA-256 and the binary's self-reported gitCommit, with one row per
# check: id, kind, expected, observed, pass.
# Live runs include the near-tie pack stage and retain its raw captures for replay.
#
# Exit: 0 every check passed, 1 a check failed, 2 self-test failure,
#       3 environment error or incomplete verdict (checksum, download, missing
#       tool, missing model, neural precondition false).
#
# The work directory is kept and its path printed on stderr; nothing is
# deleted by this script.

set -uo pipefail

SCHEMA="ee.release_probe.v1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PACK_STAGE_REQUIRED=0
# Captured before HOME is redirected into the isolated tree.
OPERATOR_HOME="${HOME:-}"

die_env() {
    printf 'release_binary_probe: %s\n' "$*" >&2
    exit 3
}

need() {
    command -v "$1" >/dev/null 2>&1 || die_env "required tool '$1' is not on PATH"
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

# Resolve symlinks in the path itself. On macOS /tmp and /var are symlinks, and
# ee's store paths disagree with a symlinked workspace root.
physical_dir() {
    (cd "$1" && pwd -P)
}

# --------------------------------------------------------------------------
# Fixed probe corpus. The first memory is the direct-hit target for search,
# pack and ask; the rest are plausible competitors so a top-hit check can fail.
# --------------------------------------------------------------------------
MEMORIES=(
    "Run cargo fmt --check before every release tag."
    "Never force-push to main; open a revert commit instead."
    "Pin the nightly toolchain in rust-toolchain.toml."
    "Use RCH for cargo builds on shared hosts."
    "Record the pack hash in every release note."
    "Keep degraded codes in the failure-mode catalog."
    "Rebuild the search index after bulk imports."
    "Prefer git commit --only in a shared checkout."
)
SKELETON_QUERY="run cargo fmt check before every release tag"
PACK_QUERY="format before release"
ASK_QUESTION="Which command must run before every release tag?"
FAILURE_MEMORY="Release tagging failed because flaky zebra-lattice tests were not quarantined."
RULE_TEXT="Quarantine flaky zebra-lattice tests before tagging a release."
RULE_SEARCH_QUERY="zebra-lattice flaky tests"
RULE_PACK_QUERY="tagging a release with flaky zebra-lattice tests"
UNRELATED_QUERY="kubernetes helm chart ingress annotations"
PARALLEL_SEARCHES=8
PARALLEL_PACKS=6
# The learn loop: three structured failures of one command in one session.
LEARN_TEXT="quartz-ledger integration shard failed: sqlite busy on the shared fixture db"
LEARN_CMD="cargo test --test quartz_ledger"
LEARN_STDERR="Error: database is locked (SQLITE_BUSY) on fixtures/quartz.db"
LEARN_PACK_QUERY="fix the flaky quartz-ledger integration shard"
CONCURRENT_WRITERS=6
PROBE_USER_AGENT="ee-release-probe (+https://github.com/Dicklesworthstone/eidetic_engine_cli)"

# Run one ee command, capturing stdout to <cap>/<name>.json and the exit code
# to <cap>/<name>.exit. stderr goes to <cap>/<name>.stderr.
capture() {
    local name="$1"
    shift
    "$EE_BIN" "$@" >"$CAP/$name.json" 2>"$CAP/$name.stderr"
    printf '%s\n' "$?" >"$CAP/$name.exit"
}

run_probe() {
    local ws="$WORK/workspace"
    mkdir -p "$ws"
    ws="$(physical_dir "$ws")"

    capture version version --json
    capture init init --workspace "$ws" --json
    # A copied model directory is stale from birth: its .verified receipt binds
    # inode and ctime, so every later process would re-hash 512 MB and the
    # probe would measure a path users only hit after a chmod or chown. Re-mint
    # the receipt the way users do (the documented repair), with downloads
    # still off. It needs a store, so it runs after init.
    if [ -d "$XDG_DATA_HOME/ee/models/potion-multilingual-128M" ]; then
        capture model_fetch model fetch embedding-default --workspace "$ws" --json
    fi
    local i=0 text
    for text in "${MEMORIES[@]}"; do
        i=$((i + 1))
        capture "remember_$i" remember "$text" --workspace "$ws" \
            --level procedural --kind rule --json
    done
    capture index_rebuild index rebuild --workspace "$ws" --json
    capture search search "$SKELETON_QUERY" --workspace "$ws" --json
    capture pack pack "$PACK_QUERY" --workspace "$ws" --max-tokens 2000 --json
    local first_id
    first_id="$(jq -r '.data.memoryId // empty' "$CAP/remember_1.json" 2>/dev/null)"
    capture why why "${first_id:-missing-memory-id}" --workspace "$ws" --json
    capture status status --workspace "$ws" --json
    capture index_status index status --workspace "$ws" --json
    capture doctor doctor --workspace "$ws" --json

    # Concurrency: identical requests launched together must agree exactly.
    local pids=() n
    for n in $(seq 1 "$PARALLEL_SEARCHES"); do
        capture "parallel_search_$n" search "$PACK_QUERY" --workspace "$ws" --json &
        pids+=("$!")
    done
    for n in $(seq 1 "$PARALLEL_PACKS"); do
        capture "parallel_pack_$n" pack "$PACK_QUERY" --workspace "$ws" \
            --max-tokens 2000 --read-only --json &
        pids+=("$!")
    done
    wait "${pids[@]}"

    capture ask ask "$ASK_QUESTION" --workspace "$ws" --json
    capture unrelated search "$UNRELATED_QUERY" --workspace "$ws" --json

    # The bd-3h6bz loop: failure evidence -> validated rule -> pack.
    capture failure_memory remember "$FAILURE_MEMORY" --workspace "$ws" \
        --level episodic --kind failure --json
    local failure_id
    failure_id="$(jq -r '.data.memoryId // empty' "$CAP/failure_memory.json" 2>/dev/null)"
    capture rule_add rule add "$RULE_TEXT" --maturity validated \
        --source-memory "${failure_id:-missing-memory-id}" --workspace "$ws" --json
    capture rule_search search "$RULE_SEARCH_QUERY" --workspace "$ws" --json
    capture rule_pack pack "$RULE_PACK_QUERY" --workspace "$ws" --max-tokens 2000 --json

    EE_EMBED_MODEL_DIR=/nonexistent capture model_missing search "$PACK_QUERY" \
        --workspace "$ws" --json

    # The learn loop through the public CLI, after every earlier check so its
    # memories cannot perturb them: repeated failure evidence -> journal
    # distill -> curation candidate -> validate -> apply -> a LATER read-only
    # pack carries the learned memory.
    for n in 1 2 3; do
        capture "journal_$n" journal append "$LEARN_TEXT" --workspace "$ws" \
            --source manual --cmd "$LEARN_CMD" --exit-code 101 \
            --stderr-tail "$LEARN_STDERR" --session probe-learn --json
    done
    capture distill journal distill --workspace "$ws" --session probe-learn --apply --json
    local candidate
    candidate="$(jq -r '.data.applied.candidateIds[0] // empty' "$CAP/distill.json" 2>/dev/null)"
    capture curate_validate curate validate "${candidate:-missing-candidate}" --workspace "$ws" --json
    capture curate_apply curate apply "${candidate:-missing-candidate}" --workspace "$ws" --json
    capture learn_pack pack "$LEARN_PACK_QUERY" --workspace "$ws" --max-tokens 2000 \
        --read-only --json

    # Concurrent writers: simultaneous remembers against one workspace while
    # read-only packs run; every write must land with its own id.
    local wpids=() k
    for k in $(seq 1 "$CONCURRENT_WRITERS"); do
        capture "cwrite_$k" remember "Concurrent probe lesson $k: keep release probes isolated." \
            --workspace "$ws" --level procedural --kind rule --json &
        wpids+=("$!")
    done
    for k in 1 2 3; do
        capture "cwrite_pack_$k" pack "$PACK_QUERY" --workspace "$ws" --max-tokens 2000 \
            --read-only --json &
        wpids+=("$!")
    done
    wait "${wpids[@]}"
    capture cwrite_list memory list --workspace "$ws" --limit 200 --json

    python3 "$SCRIPT_DIR/lib/release_pack_determinism.py" capture \
        --binary "$EE_BIN" --work "$WORK/near-tie" --captures "$CAP/near-tie" \
        >"$CAP/near-tie.result.json" 2>"$CAP/near-tie.stderr"
}

# --------------------------------------------------------------------------
# Grading. Reads only $CAP. Every check emits exactly one row.
# --------------------------------------------------------------------------
ROWS=""

row() {
    local id="$1" kind="$2" expected="$3" observed="$4" pass="$5"
    local esc_expected="${expected//\\/\\\\}" esc_observed="${observed//\\/\\\\}"
    esc_expected="${esc_expected//\"/\\\"}"
    esc_observed="${esc_observed//\"/\\\"}"
    local one
    printf -v one '{"id":"%s","kind":"%s","expected":"%s","observed":"%s","pass":%s}' \
        "$id" "$kind" "$esc_expected" "$esc_observed" "$pass"
    ROWS="${ROWS}${one}"$'\n'
    if [ "$kind" = "precondition" ] && [ "$pass" = "false" ]; then
        GRADE_PRE_FAIL=$((GRADE_PRE_FAIL + 1))
    elif [ "$kind" = "check" ] && [ "$pass" = "false" ]; then
        GRADE_FAILED="${GRADE_FAILED:+${GRADE_FAILED},}${id}"
    fi
}

# jq over one captured response; prints nothing when the file is absent or
# not JSON, so an empty world reads as a failure, never as a match.
q() {
    local _out=""
    q_into _out "$1" "$2"
    [ -n "$_out" ] && printf '%s\n' "$_out" || true
}

q_into() {
    local _var="$1" name="$2" filter="$3" raw=""
    printf -v "$_var" '%s' ""
    [ -s "$CAP/$name.json" ] || return 0
    IFS= read -r -d '' raw <"$CAP/$name.json" || true
    raw="${raw%$'\n'}"
    if [[ "$raw" != *$'\n'* && "$raw" == \{*\} ]]; then
        case "$filter" in
            '.data.memoryId // empty')
                if [[ "$raw" =~ \"memoryId\":\"([^\"]*)\" ]]; then
                    printf -v "$_var" '%s' "${BASH_REMATCH[1]}"
                    return 0
                fi
                ;;
            '.data.ruleId // .data.id // empty')
                if [[ "$raw" =~ \"ruleId\":\"([^\"]*)\" ]]; then
                    printf -v "$_var" '%s' "${BASH_REMATCH[1]}"
                    return 0
                fi
                ;;
            '.data.embed_backend // empty')
                if [[ "$raw" =~ \"embed_backend\":\"([^\"]*)\" ]]; then
                    printf -v "$_var" '%s' "${BASH_REMATCH[1]}"
                    return 0
                fi
                ;;
            '.data.posture.overall // empty')
                if [[ "$raw" =~ \"overall\":\"([^\"]*)\" ]]; then
                    printf -v "$_var" '%s' "${BASH_REMATCH[1]}"
                    return 0
                fi
                ;;
            '.data.posture // empty')
                if [[ "$raw" =~ \"posture\":\"([^\"]*)\" ]]; then
                    printf -v "$_var" '%s' "${BASH_REMATCH[1]}"
                    return 0
                fi
                ;;
            '.data.application.createdMemoryId // empty')
                if [[ "$raw" =~ \"createdMemoryId\":\"([^\"]*)\" ]]; then
                    printf -v "$_var" '%s' "${BASH_REMATCH[1]}"
                    return 0
                fi
                ;;
            '.data.build.targetTriple // empty')
                if [[ "$raw" =~ \"targetTriple\":\"([^\"]*)\" ]]; then
                    printf -v "$_var" '%s' "${BASH_REMATCH[1]}"
                    return 0
                fi
                ;;
            '.data.abstained | tostring')
                if [[ "$raw" =~ \"abstained\":(true|false) ]]; then
                    printf -v "$_var" '%s' "${BASH_REMATCH[1]}"
                    return 0
                fi
                ;;
            '[.data.citations[]?.memoryId] | join(",")')
                if [[ "$raw" =~ \"citations\":\[\{\"memoryId\":\"([^\"]*)\"\}\] ]]; then
                    printf -v "$_var" '%s' "${BASH_REMATCH[1]}"
                    return 0
                fi
                ;;
            '.data.results[0] | (.docId // .memoryId // .id) // empty')
                if [[ "$raw" =~ \"results\":\[\{\"(docId|memoryId|id)\":\"([^\"]*)\" ]]; then
                    printf -v "$_var" '%s' "${BASH_REMATCH[2]}"
                    return 0
                fi
                ;;
            '.data.resultCount // (.data.results // [] | length)')
                if [[ "$raw" =~ \"resultCount\":([0-9]+) ]]; then
                    printf -v "$_var" '%s' "${BASH_REMATCH[1]}"
                    return 0
                fi
                ;;
            '(.data.pack.items // []) | length')
                if [[ "$raw" == *'"items":[]'* ]]; then
                    printf -v "$_var" '%s' "0"
                    return 0
                elif [[ "$raw" == *'"items":[{"memoryId":"'* && "$raw" != *'},{'* ]]; then
                    printf -v "$_var" '%s' "1"
                    return 0
                fi
                ;;
        esac
    fi
    local _jq_val=""
    _jq_val="$(jq -r "$filter" "$CAP/$name.json" 2>/dev/null || true)"
    printf -v "$_var" '%s' "$_jq_val"
}

read_raw_into() {
    local _var="$1" _file="$2" _val=""
    if [ -s "$_file" ]; then
        IFS= read -r -d '' _val <"$_file" || true
        _val="${_val%$'\n'}"
    fi
    printf -v "$_var" '%s' "$_val"
}

exit_into() {
    local _var="$1" _name="$2" _v="missing"
    if [ -f "$CAP/$_name.exit" ]; then
        IFS= read -r _v <"$CAP/$_name.exit" || true
    fi
    printf -v "$_var" '%s' "$_v"
}

exit_of() {
    local _v=""
    exit_into _v "$1"
    printf '%s' "$_v"
}

ok_response() {
    [ -f "$CAP/$1.exit" ] && [ -s "$CAP/$1.json" ] || return 1
    local ex="" raw=""
    IFS= read -r ex <"$CAP/$1.exit" || true
    [ "$ex" = "0" ] || return 1
    IFS= read -r -d '' raw <"$CAP/$1.json" || true
    if [[ "$raw" == *'"schema":"ee.response.v2"'* && "$raw" == *'"success":true'* ]]; then
        return 0
    fi
    [ "$(q "$1" '(.schema == "ee.response.v2" and .success == true)')" = "true" ]
}

codes_into() {
    local _var="$1" _name="$2" raw=""
    printf -v "$_var" '%s' ""
    [ -s "$CAP/$_name.json" ] || return 0
    IFS= read -r -d '' raw <"$CAP/$_name.json" || true
    if [[ "$raw" == *'"degraded":[]'* && "$raw" != *'"degraded":['*\{* ]]; then
        return 0
    fi
    if [[ "$raw" =~ \"degraded\":\[\{\"code\":\"([^\"]+)\"\}\] && "$raw" != *'"data":{"degraded"'* ]]; then
        printf -v "$_var" '%s' "${BASH_REMATCH[1]}"
        return 0
    fi
    q_into "$_var" "$_name" '[(.degraded // [])[]?.code, (.data.degraded // [])[]?.code] | unique | join(",")'
}

codes_of() {
    local _c=""
    codes_into _c "$1"
    [ -n "$_c" ] && printf '%s\n' "$_c" || true
}

grade() {
    ROWS=""
    GRADE_PRE_FAIL=0
    GRADE_FAILED=""
    GRADE_VERDICT="pass"
    local first_id="" rule_id="" backend="" neural_pass="false"
    q_into first_id remember_1 '.data.memoryId // empty'
    q_into rule_id rule_add '.data.ruleId // .data.id // empty'

    # PRECONDITION: the default neural configuration is what was calibrated.
    q_into backend search '.data.embed_backend // empty'
    [ "$backend" = "neural_local" ] && neural_pass="true"
    row neural_backend_active precondition "search embed_backend == neural_local" \
        "embed_backend=${backend:-<absent>}" "$neural_pass"

    # 1. Walking skeleton.
    local failed_steps="" step ex_val=""
    for step in init remember_1 remember_2 remember_3 remember_4 remember_5 \
        remember_6 remember_7 remember_8 index_rebuild search pack why status \
        index_status doctor; do
        if ! ok_response "$step"; then
            exit_into ex_val "$step"
            failed_steps="${failed_steps}${step}(exit=${ex_val}) "
        fi
    done
    local top="" pack_items=""
    q_into top search '.data.results[0] | (.docId // .memoryId // .id) // empty'
    q_into pack_items pack '(.data.pack.items // []) | length'
    local skeleton_pass=false
    if [ -z "$failed_steps" ] && [ -n "$first_id" ] && [ "$top" = "$first_id" ] &&
        [ "${pack_items:-0}" -ge 1 ] 2>/dev/null; then
        skeleton_pass=true
    fi
    row walking_skeleton check \
        "16 steps exit 0 with ee.response.v2; search top hit is the first remembered memory; pack has >=1 item" \
        "failed=[${failed_steps% }] top=${top:-<none>} first=${first_id:-<none>} packItems=${pack_items:-<none>}" \
        "$skeleton_pass"

    # 2. Parallel searches agree on order and score.
    local n sig first_sig="" ps1_raw="" raw_n search_ok=true distinct=0 seen="" s_failed="" err_code=""
    for ((n = 1; n <= PARALLEL_SEARCHES; n++)); do
        if ! ok_response "parallel_search_$n"; then
            search_ok=false
            exit_into ex_val "parallel_search_$n"
            q_into err_code "parallel_search_$n" '.error.code // empty'
            s_failed="${s_failed}$n:exit=${ex_val}:${err_code} "
        fi
        read_raw_into raw_n "$CAP/parallel_search_$n.json"
        if [ -z "$raw_n" ]; then
            sig=""
        elif [ "$n" -gt 1 ] && [ "$raw_n" = "$ps1_raw" ]; then
            sig="$first_sig"
        elif [ -n "${_CACHE_PS_RAW:-}" ] && [ "$raw_n" = "$_CACHE_PS_RAW" ]; then
            sig="$_CACHE_PS_SIG"
            [ "$n" -eq 1 ] && ps1_raw="$raw_n"
        else
            q_into sig "parallel_search_$n" '[.data.results[]? | [(.docId // .memoryId // .id), .score]] | tostring'
            _CACHE_PS_RAW="$raw_n"
            _CACHE_PS_SIG="$sig"
            [ "$n" -eq 1 ] && ps1_raw="$raw_n"
        fi
        case "$sig" in "" | "[]") search_ok=false ;; esac
        case " $seen " in *" $sig "*) ;; *) seen="$seen $sig"; distinct=$((distinct + 1)) ;; esac
        [ -z "$first_sig" ] && first_sig="$sig"
    done
    [ "$distinct" -eq 1 ] || search_ok=false
    row search_deterministic_parallel check \
        "$PARALLEL_SEARCHES parallel identical searches return one non-empty ordered id+score list" \
        "distinctResultLists=$distinct failed=[${s_failed% }]" "$search_ok"

    # 3. Parallel read-only packs agree on hash and items.
    local pack_ok=true hashes="" h first_h="" pp1_raw="" pdistinct=0 p_failed=""
    for ((n = 1; n <= PARALLEL_PACKS; n++)); do
        if ! ok_response "parallel_pack_$n"; then
            pack_ok=false
            exit_into ex_val "parallel_pack_$n"
            q_into err_code "parallel_pack_$n" '.error.code // empty'
            p_failed="${p_failed}$n:exit=${ex_val}:${err_code} "
        fi
        read_raw_into raw_n "$CAP/parallel_pack_$n.json"
        if [ -z "$raw_n" ]; then
            h=""
        elif [ "$n" -gt 1 ] && [ "$raw_n" = "$pp1_raw" ]; then
            h="$first_h"
        elif [ -n "${_CACHE_PP_RAW:-}" ] && [ "$raw_n" = "$_CACHE_PP_RAW" ]; then
            h="$_CACHE_PP_H"
            [ "$n" -eq 1 ] && pp1_raw="$raw_n" && first_h="$h"
        else
            q_into h "parallel_pack_$n" '(.data.pack.hash // "") + "|" + ([(.data.pack.items // [])[]? | (.memoryId // .id)] | tostring)'
            _CACHE_PP_RAW="$raw_n"
            _CACHE_PP_H="$h"
            [ "$n" -eq 1 ] && pp1_raw="$raw_n" && first_h="$h"
        fi
        case "$h" in "" | "|"* | *"|[]") pack_ok=false ;; esac
        case " $hashes " in *" $h "*) ;; *) hashes="$hashes $h"; pdistinct=$((pdistinct + 1)) ;; esac
    done
    [ "$pdistinct" -eq 1 ] || pack_ok=false
    row pack_deterministic_parallel check \
        "$PARALLEL_PACKS parallel --read-only packs share one non-empty hash and item list" \
        "distinctHashItemSets=$pdistinct failed=[${p_failed% }]" "$pack_ok"

    # 4. Ask answers a direct hit and cites it.
    local abstained="" cited="" ask_ok=false
    q_into abstained ask '.data.abstained | tostring'
    q_into cited ask '[.data.citations[]?.memoryId] | join(",")'
    if ok_response ask && [ "$abstained" = "false" ] && [ -n "$first_id" ] &&
        case ",$cited," in *",$first_id,"*) true ;; *) false ;; esac; then
        ask_ok=true
    fi
    row ask_direct_hit check \
        "ask does not abstain and cites the first remembered memory" \
        "abstained=${abstained:-<absent>} citations=[${cited}]" "$ask_ok"

    # 5. The validated rule is searchable (shipped-good in v0.15.2).
    local rule_top="" rule_search_ok=false
    q_into rule_top rule_search '.data.results[0] | (.docId // .memoryId // .id) // empty'
    if ok_response rule_add && ok_response rule_search && [ -n "$rule_id" ] &&
        [ "$rule_top" = "$rule_id" ]; then
        rule_search_ok=true
    fi
    row rule_searchable check \
        "rule add --source-memory succeeds and the rule is the top search hit" \
        "rule=${rule_id:-<none>} top=${rule_top:-<none>}" "$rule_search_ok"

    # 6. bd-3h6bz: the rule packs beside its source memory. Since 0a42bdc6b a
    # packed rule sits under its SOURCE memory's id, so the rule id never
    # appears as a bare value (bd-p4ia2). Identify the rule by what the pack
    # emits: a procedural_rules item whose content IS the rule text AND whose
    # provenance note or why names this rule id as a whole token. The source
    # memory's own text differs from RULE_TEXT, and the id is a per-run ULID,
    # so a pack without the rule matches neither.
    local matched=0 rule_pack_ok=false rp_raw="" rp_items=""
    read_raw_into rp_raw "$CAP/rule_pack.json"
    if [ -n "$rule_id" ] && [ -n "$rp_raw" ]; then
        if [ -n "${_CACHE_RP_KEY:-}" ] && [ "${rule_id}|${rp_raw}" = "$_CACHE_RP_KEY" ]; then
            matched="$_CACHE_RP_MATCHED"
            rp_items="$_CACHE_RP_ITEMS"
        else
            matched="$(jq -r --arg r "$rule_id" --arg t "$RULE_TEXT" '
                ("(^|[^A-Za-z0-9_])" + $r + "($|[^A-Za-z0-9_])") as $tok
                | [(.data.pack.items // [])[]
                    | select(.section == "procedural_rules" and .content == $t)
                    | select(any(((.provenance // [])[]?.note), .why;
                        type == "string" and test($tok)))]
                | length' "$CAP/rule_pack.json" 2>/dev/null || echo 0)"
            q_into rp_items rule_pack '(.data.pack.items // []) | length'
            _CACHE_RP_KEY="${rule_id}|${rp_raw}"
            _CACHE_RP_MATCHED="$matched"
            _CACHE_RP_ITEMS="$rp_items"
        fi
    else
        q_into rp_items rule_pack '(.data.pack.items // []) | length'
    fi
    if ok_response rule_pack && [ -n "$rule_id" ] && [ "${matched:-0}" -ge 1 ] 2>/dev/null; then
        rule_pack_ok=true
    fi
    row rule_packs_beside_source check \
        "a procedural_rules item carries the rule text and names the rule id (bd-3h6bz, bd-p4ia2)" \
        "rule=${rule_id:-<none>} matchingItems=${matched:-0} packItems=${rp_items}" \
        "$rule_pack_ok"

    # 7. An unrelated query abstains instead of admitting everything.
    local count="" codes="" unrelated_ok=false
    q_into count unrelated '.data.resultCount // (.data.results // [] | length)'
    codes_into codes unrelated
    if ok_response unrelated; then
        if [ "${count:-x}" = "0" ]; then
            unrelated_ok=true
        else
            case ",$codes," in
                *",no_relevant_results,"* | *",weak_query_recall,"*) unrelated_ok=true ;;
            esac
        fi
    fi
    row unrelated_query_abstains check \
        "unrelated query returns 0 results or emits no_relevant_results / weak_query_recall" \
        "resultCount=${count:-<absent>} degraded=[${codes}]" "$unrelated_ok"

    # 8. status and doctor agree on whether the store is healthy.
    local s_overall="" d_posture="" agree=false
    q_into s_overall status '.data.posture.overall // empty'
    q_into d_posture doctor '.data.posture // empty'
    if ok_response status && ok_response doctor && [ -n "$s_overall" ] && [ -n "$d_posture" ]; then
        if { [ "$s_overall" = "ok" ] && [ "$d_posture" = "ok" ]; } ||
            { [ "$s_overall" != "ok" ] && [ "$d_posture" != "ok" ]; }; then
            agree=true
        fi
    fi
    row status_doctor_agree check \
        "status posture.overall is ok exactly when doctor posture is ok" \
        "status=${s_overall:-<absent>} doctor=${d_posture:-<absent>}" "$agree"

    # 9. A missing model degrades honestly.
    local mm_backend="" mm_codes="" mm_ok=false
    q_into mm_backend model_missing '.data.embed_backend // empty'
    codes_into mm_codes model_missing
    if ok_response model_missing && [ "$mm_backend" = "hash_fallback" ]; then
        case ",$mm_codes," in *",embed_model_unavailable,"*) mm_ok=true ;; esac
    fi
    row model_missing_fallback check \
        "EE_EMBED_MODEL_DIR=/nonexistent yields hash_fallback plus embed_model_unavailable" \
        "embed_backend=${mm_backend:-<absent>} degraded=[${mm_codes}]" "$mm_ok"

    # 10. The model receipt is fresh after the documented re-mint, so the other
    # rows ran on the path users run, not a re-hash-every-process path. A
    # fetch that was attempted must succeed; status must be a real response.
    local fetch_ok=true st_codes="" mf_exit="" receipt_ok=false
    if [ -f "$CAP/model_fetch.exit" ] && ! ok_response model_fetch; then fetch_ok=false; fi
    codes_into st_codes status
    exit_into mf_exit model_fetch
    if $fetch_ok && ok_response status; then
        case ",$st_codes," in *",embed_model_receipt_stale,"*) ;; *) receipt_ok=true ;; esac
    fi
    row model_receipt_fresh check \
        "model fetch (when a model is present) succeeds and status carries no embed_model_receipt_stale" \
        "modelFetch=${mf_exit} status.degraded=[${st_codes}]" "$receipt_ok"

    # 11. The learn loop: evidence becomes a memory that a later pack uses.
    local learned="" learn_hits=0 learn_ok=false step failed_learn="" lp_raw=""
    for step in journal_1 journal_2 journal_3 distill curate_validate curate_apply learn_pack; do
        if ! ok_response "$step"; then
            exit_into ex_val "$step"
            failed_learn="${failed_learn}${step}(exit=${ex_val}) "
        fi
    done
    q_into learned curate_apply '.data.application.createdMemoryId // empty'
    if [ -n "$learned" ] && [ -s "$CAP/learn_pack.json" ]; then
        read_raw_into lp_raw "$CAP/learn_pack.json"
        if [[ "$lp_raw" == *"\"memoryId\":\"$learned\""* || "$lp_raw" == *"\"id\":\"$learned\""* ]]; then
            learn_hits=1
        fi
    fi
    if [ -z "$failed_learn" ] && [ -n "$learned" ] && [ "${learn_hits:-0}" -ge 1 ] 2>/dev/null; then
        learn_ok=true
    fi
    row learn_loop_applies check \
        "3 journal failures -> distill --apply -> curate validate+apply creates a memory that a later read-only pack carries" \
        "failed=[${failed_learn% }] learned=${learned:-<none>} inLaterPack=${learn_hits:-0}" "$learn_ok"

    # 12. Concurrent writers: every simultaneous remember lands, with its own id.
    local ids="" id="" w_failed="" w_distinct=0 w_listed=0 w_ok=false k cwrite_list_raw=""
    read_raw_into cwrite_list_raw "$CAP/cwrite_list.json"
    for ((k = 1; k <= CONCURRENT_WRITERS; k++)); do
        if ok_response "cwrite_$k"; then
            q_into id "cwrite_$k" '.data.memoryId // empty'
            case " $ids " in *" $id "*) ;; *) [ -n "$id" ] && ids="$ids $id" && w_distinct=$((w_distinct + 1)) ;; esac
            if [ -n "$id" ] && [[ "$cwrite_list_raw" == *"\"id\":\"$id\""* ]]; then
                w_listed=$((w_listed + 1))
            fi
        else
            exit_into ex_val "cwrite_$k"
            w_failed="${w_failed}cwrite_$k(exit=${ex_val}) "
        fi
    done
    for k in 1 2 3; do
        if ! ok_response "cwrite_pack_$k"; then
            exit_into ex_val "cwrite_pack_$k"
            w_failed="${w_failed}cwrite_pack_$k(exit=${ex_val}) "
        fi
    done
    if [ -z "$w_failed" ] && [ "$w_distinct" -eq "$CONCURRENT_WRITERS" ] &&
        [ "$w_listed" -eq "$CONCURRENT_WRITERS" ] && ok_response cwrite_list; then
        w_ok=true
    fi
    row concurrent_writes_intact check \
        "$CONCURRENT_WRITERS simultaneous remembers (with 3 read-only packs running) all succeed with distinct ids, all listed afterwards" \
        "failed=[${w_failed% }] distinctIds=$w_distinct listed=$w_listed" "$w_ok"

    # 13. Target triple binding (when --expected-target is supplied or inferred
    # from ee-<target>.tar.*): one target's capture cannot stand in for another's.
    local want_target="${EXPECTED_TARGET:-}"
    if [ -z "$want_target" ] && [ -f "$CAP/expected_target" ]; then
        IFS= read -r want_target <"$CAP/expected_target" || true
    fi
    if [ -n "$want_target" ]; then
        local got_target="" target_ok=false
        q_into got_target version '.data.build.targetTriple // empty'
        if ok_response version && [ "$got_target" = "$want_target" ]; then
            target_ok=true
        fi
        row target_triple_matches check \
            "binary self-reported targetTriple matches archive target $want_target" \
            "targetTriple=${got_target:-<absent>} expected=$want_target" "$target_ok"
    fi

    if [ "$PACK_STAGE_REQUIRED" = 1 ] || [ -d "$CAP/near-tie" ]; then
        local near_tie="" near_tie_ok=false commit="" binary_sha=""
        q_into commit version '.data.source.gitCommit // empty'
        if [ -f "$CAP/binary.sha256" ]; then
            IFS= read -r binary_sha <"$CAP/binary.sha256" || true
        fi
        if near_tie="$(python3 "$SCRIPT_DIR/lib/release_pack_determinism.py" grade \
            --captures "$CAP/near-tie")" && [ -n "$commit" ] && [ -n "$binary_sha" ] &&
            jq -e --arg commit "$commit" --arg sha "$binary_sha" \
                '.pass == true and .sourceCommit == $commit and .binarySha256 == $sha' \
                <<<"$near_tie" >/dev/null; then
            near_tie_ok=true
        fi
        row pack_near_tie_determinism check \
            "12 live and 12 fixed packs in each mode preserve ordered typed decisions and durable source state" \
            "${near_tie:-<missing pack stage>}" "$near_tie_ok"
    fi
    if [ "$GRADE_PRE_FAIL" -gt 0 ]; then
        GRADE_VERDICT="incomplete"
    elif [ -n "$GRADE_FAILED" ]; then
        GRADE_VERDICT="fail"
    else
        GRADE_VERDICT="pass"
    fi
}

# Assemble the verdict object from ROWS. Precondition false -> incomplete.
verdict_json() {
    local archive_src="$1" archive_sha="$2" binary_sha=""
    if [ -f "$CAP/binary.sha256" ]; then
        IFS= read -r binary_sha <"$CAP/binary.sha256" || true
    fi
    printf '%s' "$ROWS" | jq -cs \
        --arg schema "$SCHEMA" --arg src "$archive_src" --arg sha "$archive_sha" \
        --arg binary_sha "$binary_sha" \
        --slurpfile version <(cat "$CAP/version.json" 2>/dev/null || echo '{}') '
        ($version[0].data // {}) as $v
        | (map(select(.kind == "precondition" and .pass == false)) | length) as $pre
        | (map(select(.kind == "check" and .pass == false)) | map(.id)) as $failed
        | {
            schema: $schema,
            archive: {source: $src, sha256: $sha},
            binary: {
              version: ($v.version // null),
              gitCommit: ($v.source.gitCommit // null),
              gitTag: ($v.source.gitTag // null),
              gitDirty: $v.source.gitDirty,
              sha256: (if $binary_sha == "" then null else $binary_sha end),
              targetTriple: ($v.build.targetTriple // null)
            },
            verdict: (if $pre > 0 then "incomplete"
                      elif ($failed | length) > 0 then "fail" else "pass" end),
            failed: $failed,
            checks: .
          }'
}

print_human() {
    printf '%s' "$1" | jq -r '
        "release probe: \(.verdict)  ee \(.binary.version // "?") gitCommit=\(.binary.gitCommit // "?")",
        "archive sha256: \(.archive.sha256)",
        (.checks[] | "  \(if .pass then "PASS" else "FAIL" end)  \(.id)  [\(.kind)]  observed: \(.observed)")'
}

emit() {
    local verdict_obj="$1" verdict
    verdict="$(printf '%s' "$verdict_obj" | jq -r .verdict)"
    if [ -n "${OUTPUT_FILE:-}" ] && [ "$verdict" = "pass" ]; then
        mkdir -p "$(dirname "$OUTPUT_FILE")" || die_env "cannot create parent directory for $OUTPUT_FILE"
        printf '%s\n' "$verdict_obj" >"$OUTPUT_FILE" || die_env "cannot write $OUTPUT_FILE"
    fi
    if [ "$JSON" = 1 ]; then
        printf '%s\n' "$verdict_obj"
    else
        print_human "$verdict_obj"
    fi
    case "$verdict" in
        pass) exit 0 ;;
        fail) exit 1 ;;
        *) exit 3 ;;
    esac
}

# --------------------------------------------------------------------------
# Self-test: build one captured world where every check passes, then break
# each check in turn and prove that exactly that check fails. Also prove the
# empty world and a false precondition cannot read as a pass, and that a
# checksum mismatch refuses to execute.
# --------------------------------------------------------------------------
fixture_ok() {
    local d="$1" i
    mkdir -p "$d"
    local mem='mem_FIRST' rule='rule_R1'
    resp() { printf '{"schema":"ee.response.v2","success":true,"data":%s,"degraded":[]}\n' "$1"; }
    for f in init index_rebuild why index_status model_fetch; do resp '{}' >"$d/$f.json"; done
    for i in 1 2 3 4 5 6 7 8; do resp "{\"memoryId\":\"mem_$i\"}" >"$d/remember_$i.json"; done
    resp "{\"memoryId\":\"$mem\"}" >"$d/remember_1.json"
    resp '{"version":"9.9.9","source":{"gitCommit":"abc","gitTag":"v9.9.9","gitDirty":false},"build":{"targetTriple":"t"}}' >"$d/version.json"
    resp "{\"embed_backend\":\"neural_local\",\"resultCount\":2,\"results\":[{\"docId\":\"$mem\",\"score\":0.5},{\"docId\":\"mem_2\",\"score\":0.4}]}" >"$d/search.json"
    resp '{"pack":{"hash":"blake3:h","items":[{"memoryId":"mem_FIRST"}]}}' >"$d/pack.json"
    for ((i = 1; i <= PARALLEL_SEARCHES; i++)); do cp "$d/search.json" "$d/parallel_search_$i.json"; done
    for ((i = 1; i <= PARALLEL_PACKS; i++)); do cp "$d/pack.json" "$d/parallel_pack_$i.json"; done
    resp "{\"abstained\":false,\"citations\":[{\"memoryId\":\"$mem\"}]}" >"$d/ask.json"
    resp '{"resultCount":0,"results":[]}' >"$d/unrelated.json"
    resp '{"memoryId":"mem_FAIL"}' >"$d/failure_memory.json"
    resp "{\"ruleId\":\"$rule\"}" >"$d/rule_add.json"
    resp "{\"results\":[{\"docId\":\"$rule\",\"score\":0.9}]}" >"$d/rule_search.json"
    # The shape a real pack emits since 0a42bdc6b (bd-p4ia2): the rule sits
    # under its SOURCE memory's id, carries RULE_TEXT, and names the rule id
    # only inside its provenance note and why.
    jq -cn --arg rule "$rule" --arg text "$RULE_TEXT" '{schema:"ee.response.v2", success:true, degraded:[],
        data:{pack:{hash:"blake3:r", items:[
            {section:"failures", memoryId:"mem_FAIL", content:"Release tagging failed."},
            {section:"procedural_rules", memoryId:"mem_FAIL", content:$text,
             provenance:[{note:"Memory mem_FAIL selected for context pack"},
                         {note:("Promoted procedural rule " + $rule)}],
             why:("matched; via applied procedural rule " + $rule + " (bd-3h6bz)")}]}}}' >"$d/rule_pack.json"
    resp '{"posture":{"overall":"ok"}}' >"$d/status.json"
    resp '{"posture":"ok","healthy":true}' >"$d/doctor.json"
    jq -cn '{schema:"ee.response.v2", success:true, data:{embed_backend:"hash_fallback"}, degraded:[{code:"embed_model_unavailable"}]}' >"$d/model_missing.json"
    for i in 1 2 3; do resp '{}' >"$d/journal_$i.json"; done
    resp '{"applied":{"candidateIds":["cand_1"]}}' >"$d/distill.json"
    resp '{}' >"$d/curate_validate.json"
    resp '{"application":{"createdMemoryId":"mem_LEARNED"}}' >"$d/curate_apply.json"
    resp '{"pack":{"hash":"blake3:l","items":[{"memoryId":"mem_LEARNED"},{"memoryId":"mem_FIRST"}]}}' >"$d/learn_pack.json"
    for i in $(seq 1 "$CONCURRENT_WRITERS"); do resp "{\"memoryId\":\"mem_C$i\"}" >"$d/cwrite_$i.json"; done
    for i in 1 2 3; do cp "$d/pack.json" "$d/cwrite_pack_$i.json"; done
    resp "{\"memories\":[$(for i in $(seq 1 "$CONCURRENT_WRITERS"); do printf '{"id":"mem_C%s"},' "$i"; done){\"id\":\"mem_FIRST\"}]}" >"$d/cwrite_list.json"
    for f in "$d"/*.json; do printf '0\n' >"${f%.json}.exit"; done
}

# mutate <dir> <file-stem> <jq filter>
mutate() {
    DIRTY_FILES+=("$2.json")
    local tmp="$1/$2.json.tmp"
    jq -c "$3" "$1/$2.json" >"$tmp" && mv "$tmp" "$1/$2.json"
}

self_test() {
    need jq
    local root failures=0 tmp_base="${TMPDIR:-/tmp}"
    if [ "$(uname -s 2>/dev/null || true)" = "Darwin" ] && [[ "$tmp_base" == /Volumes/* ]] && [ -d /private/tmp ]; then
        tmp_base="/private/tmp"
    fi
    root="$(mktemp -d "${tmp_base}/ee-release-probe-selftest.XXXXXX")"
    printf 'release_binary_probe self-test workdir: %s\n' "$root" >&2
    fixture_ok "$root/_template"
    cp -R "$root/_template" "$root/work"
    DIRTY_FILES=()

    expect() {
        # expect <label> <dir> <expected verdict> <expected failed ids csv>
        local label="$1" dir="$2" want_verdict="$3" want_failed="$4" got
        CAP="$dir"
        grade
        got="${GRADE_VERDICT} ${GRADE_FAILED}"
        if [ "$label" = "all checks pass" ] || [ "$label" = "empty captures cannot pass" ]; then
            got="$(verdict_json fixture 0 | jq -r '.verdict + " " + (.failed | join(","))')"
        fi
        if [ "$got" = "$want_verdict $want_failed" ]; then
            printf '  ok    %-40s -> %s\n' "$label" "$got" >&2
        else
            printf '  FAIL  %-40s -> got [%s] want [%s %s]\n' "$label" "$got" "$want_verdict" "$want_failed" >&2
            failures=$((failures + 1))
        fi
        if [ "$dir" = "$root/work" ]; then
            local df ex_v
            for df in "${DIRTY_FILES[@]+"${DIRTY_FILES[@]}"}"; do
                cp "$root/_template/$df" "$root/work/$df"
            done
            DIRTY_FILES=()
            for df in index_status.exit doctor.exit rule_add.exit model_fetch.exit distill.exit cwrite_4.exit cwrite_pack_2.exit; do
                IFS= read -r ex_v <"$root/work/$df" || true
                if [ "$ex_v" != "0" ]; then
                    printf '0\n' >"$root/work/$df"
                fi
            done
            [ ! -f "$root/work/expected_target" ] || rm -f "$root/work/expected_target"
        fi
    }
    case_dir() {
        case "$1" in
            empty_world | skeleton_top | no_model_no_fetch | out_bad | rel_cap)
                local d="$root/$1"
                cp -R "$root/_template" "$d"
                printf '%s' "$d"
                ;;
            *)
                printf '%s' "$root/work"
                ;;
        esac
    }

    local d
    d="$(case_dir good)"; expect "all checks pass" "$d" pass ""
    local missing_pack_stage="$root/missing-pack-stage"
    cp -R "$root/_template" "$missing_pack_stage"
    mkdir "$missing_pack_stage/near-tie"
    expect "near-tie stage without captures fails" "$missing_pack_stage" fail "pack_near_tie_determinism"

    d="$(case_dir empty_world)"; command find "$d" -name '*.json' -exec sh -c ': >"$1"' _ {} \;
    expect "empty captures cannot pass" "$d" incomplete \
        "walking_skeleton,search_deterministic_parallel,pack_deterministic_parallel,ask_direct_hit,rule_searchable,rule_packs_beside_source,unrelated_query_abstains,status_doctor_agree,model_missing_fallback,model_receipt_fresh,learn_loop_applies,concurrent_writes_intact"

    d="$(case_dir hash_backend)"; mutate "$d" search '.data.embed_backend = "hash_fallback"'
    expect "non-neural backend is incomplete" "$d" incomplete ""

    d="$(case_dir skeleton_exit)"; printf '1\n' >"$d/index_status.exit"
    expect "a skeleton step exiting 1" "$d" fail "walking_skeleton"

    # doctor feeds two checks, so one failure must surface in both.
    d="$(case_dir doctor_exit)"; printf '1\n' >"$d/doctor.exit"
    expect "doctor exiting 1" "$d" fail "walking_skeleton,status_doctor_agree"

    d="$(case_dir skeleton_top)"; mutate "$d" search '.data.results |= reverse'
    for i in $(seq 1 "$PARALLEL_SEARCHES"); do cp "$d/search.json" "$d/parallel_search_$i.json"; done
    expect "wrong top hit" "$d" fail "walking_skeleton"

    d="$(case_dir search_order)"; mutate "$d" parallel_search_5 '.data.results |= reverse'
    expect "one parallel search reordered" "$d" fail "search_deterministic_parallel"

    d="$(case_dir search_score)"; mutate "$d" parallel_search_2 '.data.results[1].score = 0.41'
    expect "one parallel search re-scored" "$d" fail "search_deterministic_parallel"

    d="$(case_dir pack_hash)"; mutate "$d" parallel_pack_6 '.data.pack.hash = "blake3:other"'
    expect "one parallel pack re-hashed" "$d" fail "pack_deterministic_parallel"

    d="$(case_dir ask_abstain)"; mutate "$d" ask '.data.abstained = true'
    expect "ask abstains" "$d" fail "ask_direct_hit"

    d="$(case_dir ask_wrong_cite)"; mutate "$d" ask '.data.citations = [{"memoryId":"mem_2"}]'
    expect "ask cites the wrong memory" "$d" fail "ask_direct_hit"

    d="$(case_dir rule_not_top)"; mutate "$d" rule_search '.data.results = [{"docId":"mem_FAIL","score":0.9}]'
    expect "rule not the top search hit" "$d" fail "rule_searchable"

    d="$(case_dir rule_not_packed)"; mutate "$d" rule_pack '.data.pack.items = [{"memoryId":"mem_FAIL"}]'
    expect "rule omitted from pack (bd-3h6bz)" "$d" fail "rule_packs_beside_source"

    # bd-p4ia2: each clause of the predicate must be load-bearing.
    d="$(case_dir rule_bare_id_only)"; mutate "$d" rule_pack '.data.pack.items = [{"memoryId":"rule_R1"}]'
    expect "rule id as a bare value, no rule text" "$d" fail "rule_packs_beside_source"

    d="$(case_dir rule_text_unlinked)"; mutate "$d" rule_pack '.data.pack.items[1] |= (.provenance = [{"note":"Memory mem_FAIL selected for context pack"}] | .why = "matched")'
    expect "rule text with no id link" "$d" fail "rule_packs_beside_source"

    d="$(case_dir rule_link_other_text)"; mutate "$d" rule_pack '.data.pack.items[1].content = "Some other procedural text."'
    expect "id link on different content" "$d" fail "rule_packs_beside_source"

    d="$(case_dir rule_wrong_section)"; mutate "$d" rule_pack '.data.pack.items[1].section = "failures"'
    expect "rule item outside procedural_rules" "$d" fail "rule_packs_beside_source"

    d="$(case_dir rule_id_prefix)"; mutate "$d" rule_pack '.data.pack.items[1] |= (.provenance[1].note = "Promoted procedural rule rule_R12" | .why = "via applied procedural rule rule_R12")'
    expect "a longer id sharing the prefix" "$d" fail "rule_packs_beside_source"

    d="$(case_dir rule_linked_by_why)"; mutate "$d" rule_pack '.data.pack.items[1].provenance = [{"note":"Memory mem_FAIL selected for context pack"}]'
    expect "rule linked by why alone" "$d" pass ""

    d="$(case_dir rule_linked_by_note)"; mutate "$d" rule_pack '.data.pack.items[1].why = "matched"'
    expect "rule linked by provenance note alone" "$d" pass ""

    d="$(case_dir rule_add_refused)"; printf '1\n' >"$d/rule_add.exit"; mutate "$d" rule_add '.success = false | .data = {}'
    expect "rule add refused" "$d" fail "rule_searchable,rule_packs_beside_source"

    d="$(case_dir unrelated_admits)"; mutate "$d" unrelated '.data.resultCount = 8 | .data.results = [range(8) | {docId:"m"}]'
    expect "unrelated query admits 8" "$d" fail "unrelated_query_abstains"

    d="$(case_dir unrelated_flagged)"; mutate "$d" unrelated '.data.resultCount = 8 | .degraded = [{"code":"no_relevant_results"}]'
    expect "unrelated query flagged no_relevant_results" "$d" pass ""

    d="$(case_dir posture_disagree)"; mutate "$d" status '.data.posture.overall = "degraded_recoverable"'
    expect "status degraded while doctor ok" "$d" fail "status_doctor_agree"

    d="$(case_dir posture_both_bad)"; mutate "$d" status '.data.posture.overall = "degraded_recoverable"'
    mutate "$d" doctor '.data.posture = "degraded"'
    expect "status and doctor both degraded" "$d" pass ""

    d="$(case_dir model_not_flagged)"; mutate "$d" model_missing '.degraded = []'
    expect "model missing but not flagged" "$d" fail "model_missing_fallback"

    d="$(case_dir model_still_neural)"; mutate "$d" model_missing '.data.embed_backend = "neural_local"'
    expect "model missing yet neural reported" "$d" fail "model_missing_fallback"

    d="$(case_dir receipt_stale)"; mutate "$d" status '.degraded = [{"code":"embed_model_receipt_stale"}]'
    expect "receipt still stale after re-mint" "$d" fail "model_receipt_fresh"

    d="$(case_dir fetch_fails)"; printf '1\n' >"$d/model_fetch.exit"; mutate "$d" model_fetch '.success = false'
    expect "model fetch (the documented repair) fails" "$d" fail "model_receipt_fresh"

    d="$(case_dir no_model_no_fetch)"; command find "$d" -name 'model_fetch.*' -exec sh -c 'mv "$1" "$1.skipped"' _ {} \;
    expect "no model present: fetch not attempted" "$d" pass ""

    d="$(case_dir learn_not_packed)"; mutate "$d" learn_pack '.data.pack.items = [{"memoryId":"mem_FIRST"}]'
    expect "learned memory absent from later pack" "$d" fail "learn_loop_applies"

    d="$(case_dir learn_nothing_created)"; mutate "$d" curate_apply '.data.application = {}'
    expect "curate apply created no memory" "$d" fail "learn_loop_applies"

    d="$(case_dir learn_distill_failed)"; printf '1\n' >"$d/distill.exit"
    expect "journal distill exiting 1" "$d" fail "learn_loop_applies"

    d="$(case_dir cwrite_one_failed)"; printf '1\n' >"$d/cwrite_4.exit"
    expect "one concurrent remember exits 1" "$d" fail "concurrent_writes_intact"

    d="$(case_dir cwrite_lost)"; mutate "$d" cwrite_list '.data.memories |= map(select(.id != "mem_C3"))'
    expect "a concurrent write missing afterwards" "$d" fail "concurrent_writes_intact"

    d="$(case_dir cwrite_dup_id)"; mutate "$d" cwrite_2 '.data.memoryId = "mem_C1"'
    expect "two writers reported one id" "$d" fail "concurrent_writes_intact"

    d="$(case_dir cwrite_pack_failed)"; printf '1\n' >"$d/cwrite_pack_2.exit"
    expect "a pack beside the writers exits 1" "$d" fail "concurrent_writes_intact"

    d="$(case_dir target_match)"; printf 't\n' >"$d/expected_target"
    expect "matching target triple" "$d" pass ""

    d="$(case_dir target_mismatch)"; printf 'aarch64-apple-darwin\n' >"$d/expected_target"
    expect "mismatched target triple rejected" "$d" fail "target_triple_matches"

    # --output writes only on pass, never on fail.
    local out_pass="$root/out-pass.json" out_fail="$root/out-fail.json" rc_out
    d="$(case_dir out_good)"
    ( "$0" --replay "$d" --output "$out_pass" --json >/dev/null ) 2>/dev/null
    rc_out=$?
    if [ "$rc_out" -eq 0 ] && [ -s "$out_pass" ] && [ "$(jq -r .verdict "$out_pass")" = "pass" ]; then
        printf '  ok    %-40s -> exit 0 + file written\n' "--output written on pass" >&2
    else
        printf '  FAIL  %-40s -> exit %s file=%s\n' "--output written on pass" "$rc_out" "$out_pass" >&2
        failures=$((failures + 1))
    fi
    d="$(case_dir out_bad)"; printf '1\n' >"$d/index_status.exit"
    ( "$0" --replay "$d" --output "$out_fail" --json >/dev/null ) 2>/dev/null
    rc_out=$?
    if [ "$rc_out" -eq 1 ] && [ ! -e "$out_fail" ]; then
        printf '  ok    %-40s -> exit 1 + no file\n' "--output suppressed on fail" >&2
    else
        printf '  FAIL  %-40s -> exit %s file_exists=%s\n' "--output suppressed on fail" "$rc_out" "$([ -e "$out_fail" ] && echo yes || echo no)" >&2
        failures=$((failures + 1))
    fi

    # --verify-release-dir positive and negative controls.
    local rdir="$root/rel-good" rsha
    mkdir -p "$rdir"
    printf 'fake archive bytes' >"$rdir/ee-aarch64-apple-darwin.tar.xz"
    rsha="$(sha256_of "$rdir/ee-aarch64-apple-darwin.tar.xz")"
    d="$(case_dir rel_cap)"
    mutate "$d" version '.data.build.targetTriple = "aarch64-apple-darwin"'
    printf 'aarch64-apple-darwin\n' >"$d/expected_target"
    printf '%s\n' "$rsha" >"$d/archive.sha256"
    ( "$0" --replay "$d" --output "$rdir/release-probe-aarch64-apple-darwin.json" --json >/dev/null ) 2>/dev/null
    ( "$0" --verify-release-dir "$rdir" ) 2>/dev/null
    rc_out=$?
    if [ "$rc_out" -eq 0 ]; then
        printf '  ok    %-40s -> exit 0\n' "verify-release-dir valid capture" >&2
    else
        printf '  FAIL  %-40s -> exit %s want 0\n' "verify-release-dir valid capture" "$rc_out" >&2
        failures=$((failures + 1))
    fi
    local rdir_wrong_triple="$root/rel-wrong-triple"
    mkdir -p "$rdir_wrong_triple"
    cp "$rdir/ee-aarch64-apple-darwin.tar.xz" "$rdir_wrong_triple/"
    jq '.binary.targetTriple = "x86_64-unknown-linux-gnu"' "$rdir/release-probe-aarch64-apple-darwin.json" \
        >"$rdir_wrong_triple/release-probe-aarch64-apple-darwin.json"
    ( "$0" --verify-release-dir "$rdir_wrong_triple" ) 2>/dev/null
    rc_out=$?
    if [ "$rc_out" -eq 1 ]; then
        printf '  ok    %-40s -> exit 1\n' "verify-release-dir wrong targetTriple" >&2
    else
        printf '  FAIL  %-40s -> exit %s want 1\n' "verify-release-dir wrong targetTriple" "$rc_out" >&2
        failures=$((failures + 1))
    fi
    local rdir_missing="$root/rel-missing"
    mkdir -p "$rdir_missing"
    cp "$rdir/ee-aarch64-apple-darwin.tar.xz" "$rdir_missing/"
    ( "$0" --verify-release-dir "$rdir_missing" ) 2>/dev/null
    rc_out=$?
    if [ "$rc_out" -eq 1 ]; then
        printf '  ok    %-40s -> exit 1\n' "verify-release-dir missing capture" >&2
    else
        printf '  FAIL  %-40s -> exit %s want 1\n' "verify-release-dir missing capture" "$rc_out" >&2
        failures=$((failures + 1))
    fi

    # --verify-release-dir --proof-capsule positive and negative controls (.76).
    local sha_good="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    local sha_other="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    local cap_repo="$root/cap_repo"
    mkdir -p "$cap_repo/scripts"
    printf 'cargo-lock\n' >"$cap_repo/Cargo.lock"
    printf 'franken-lock\n' >"$cap_repo/franken-stack.lock"
    printf 'total_expected_seconds = 608\n' >"$cap_repo/scripts/verify-budget.toml"
    local cap_bin="$root/ee-cap-bin"
    printf 'fake-ee-binary\n' >"$cap_bin"
    local cap_log="$root/cap-lib.log"
    cat >"$cap_log" <<'EOF'
     Running unittests src/lib.rs (target/debug/deps/ee-aaa)
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EOF
    local cap_log_sha
    cap_log_sha="$(sha256_of "$cap_log")"
    jq -n \
        --arg base "$sha_good" \
        --arg bin "$cap_bin" \
        --arg log "$cap_log" \
        --arg log_sha "$cap_log_sha" \
        '{
            base: $base,
            treeish: "main",
            dirty: 0,
            host: "worker-01",
            toolchain: {
                rustc: "rustc 1.90.0-nightly",
                cargo: "cargo 1.90.0-nightly",
                channel: "nightly",
                target: "aarch64-apple-darwin",
                features: ["fts5", "json", "embed-fast", "lexical-bm25", "graph"],
                evaluatedUnderCfg: "target_family=unix, target_os=darwin"
            },
            manifestPath: "scripts/verify-budget.toml",
            requiredStages: ["lib"],
            binaries: [{path: $bin, version: "0.19.0", target: "aarch64-apple-darwin", features: ["fts5"]}],
            siblingPins: [{crate: "fsqlite", resolved: "=0.4.10", path: null}],
            stages: [{
                stage: "lib",
                status: "PASS",
                exitCode: 0,
                terminatorClass: "product_verdict",
                target: "src/lib.rs",
                announced: 4,
                executed: 4,
                failed: 0,
                ignored: 0,
                workerHost: "worker-01",
                logPath: $log,
                logSha256: $log_sha,
                commit: $base
            }],
            evidencePaths: ["scripts/verify-budget.toml"],
            runIdentifiers: {rch: "self-test-run"}
        }' >"$root/cap_manifest.json"
    local cap_good="$root/good.capsule.json"
    python3 "$ROOT/scripts/lib/emit_proof_capsule.py" \
        --populate-candidate \
        --candidate-manifest "$root/cap_manifest.json" \
        --repo-root "$cap_repo" \
        --out "$cap_good" >/dev/null
    local rdir_cap="$root/rel-capsule"
    mkdir -p "$rdir_cap"
    cp "$rdir/ee-aarch64-apple-darwin.tar.xz" "$rdir_cap/"
    jq --arg sha "$sha_good" '.binary.gitCommit = $sha' "$rdir/release-probe-aarch64-apple-darwin.json" \
        >"$rdir_cap/release-probe-aarch64-apple-darwin.json"

    ( "$0" --verify-release-dir "$rdir_cap" --proof-capsule "$cap_good" ) 2>/dev/null
    rc_out=$?
    if [ "$rc_out" -eq 0 ]; then
        printf '  ok    %-40s -> exit 0\n' "verify-release-dir bound proof capsule" >&2
    else
        printf '  FAIL  %-40s -> exit %s want 0\n' "verify-release-dir bound proof capsule" "$rc_out" >&2
        failures=$((failures + 1))
    fi
    ( "$0" --verify-release-dir "$rdir_cap" --proof-capsule "$cap_good" \
        --candidate-sha "$sha_good" --candidate-version 0.19.0 ) 2>"$root/missing-pack-stage.stderr"
    rc_out=$?
    if [ "$rc_out" -eq 1 ] && grep -q 'requires a passing pack_near_tie_determinism check' \
        "$root/missing-pack-stage.stderr"; then
        printf '  ok    %-40s -> exit 1\n' "candidate requires near-tie stage" >&2
    else
        printf '  FAIL  %-40s -> exit %s want stage refusal\n' "candidate requires near-tie stage" "$rc_out" >&2
        failures=$((failures + 1))
    fi

    local rdir_cap_bad_sha="$root/rel-capsule-bad-sha"
    mkdir -p "$rdir_cap_bad_sha"
    cp "$rdir_cap/ee-aarch64-apple-darwin.tar.xz" "$rdir_cap_bad_sha/"
    jq --arg sha "$sha_other" '.binary.gitCommit = $sha' "$rdir_cap/release-probe-aarch64-apple-darwin.json" \
        >"$rdir_cap_bad_sha/release-probe-aarch64-apple-darwin.json"
    ( "$0" --verify-release-dir "$rdir_cap_bad_sha" --proof-capsule "$cap_good" ) 2>/dev/null
    rc_out=$?
    if [ "$rc_out" -eq 1 ]; then
        printf '  ok    %-40s -> exit 1\n' "verify-release-dir mismatched gitCommit" >&2
    else
        printf '  FAIL  %-40s -> exit %s want 1\n' "verify-release-dir mismatched gitCommit" "$rc_out" >&2
        failures=$((failures + 1))
    fi

    local cap_tampered="$root/tampered.capsule.json"
    jq '.results[0].workerHost = "worker-tampered"' "$cap_good" >"$cap_tampered"
    ( "$0" --verify-release-dir "$rdir_cap" --proof-capsule "$cap_tampered" ) 2>/dev/null
    rc_out=$?
    if [ "$rc_out" -eq 1 ]; then
        printf '  ok    %-40s -> exit 1\n' "verify-release-dir tampered capsule" >&2
    else
        printf '  FAIL  %-40s -> exit %s want 1\n' "verify-release-dir tampered capsule" "$rc_out" >&2
        failures=$((failures + 1))
    fi

    local cap_skeleton="$root/skeleton.capsule.json"
    python3 "$ROOT/scripts/lib/emit_proof_capsule.py" \
        --log "$cap_log" --base "$sha_good" --dirty 0 --host "worker-01" \
        --run-exit 0 --command "self-test" --out "$cap_skeleton" >/dev/null
    ( "$0" --verify-release-dir "$rdir_cap" --proof-capsule "$cap_skeleton" ) 2>/dev/null
    rc_out=$?
    if [ "$rc_out" -eq 1 ]; then
        printf '  ok    %-40s -> exit 1\n' "verify-release-dir unpopulated skeleton" >&2
    else
        printf '  FAIL  %-40s -> exit %s want 1\n' "verify-release-dir unpopulated skeleton" "$rc_out" >&2
        failures=$((failures + 1))
    fi

    local rdir_compile_empty_sha="$root/rel-compile-only-empty-sha"
    mkdir -p "$rdir_compile_empty_sha"
    cp "$rdir_cap/ee-aarch64-apple-darwin.tar.xz" "$rdir_compile_empty_sha/"
    jq '.verdict = "compile_only" | .binary.gitCommit = ""' \
        "$rdir/release-probe-aarch64-apple-darwin.json" \
        >"$rdir_compile_empty_sha/release-probe-aarch64-apple-darwin.json"
    ( "$0" --verify-release-dir "$rdir_compile_empty_sha" --allow-compile-only aarch64-apple-darwin --proof-capsule "$cap_good" ) 2>/dev/null
    rc_out=$?
    if [ "$rc_out" -eq 1 ]; then
        printf '  ok    %-40s -> exit 1\n' "verify-release-dir compile_only empty sha" >&2
    else
        printf '  FAIL  %-40s -> exit %s want 1\n' "verify-release-dir compile_only empty sha" "$rc_out" >&2
        failures=$((failures + 1))
    fi

    ( "$0" --verify-release-dir "$rdir_cap" --proof-capsule ) 2>/dev/null
    rc_out=$?
    if [ "$rc_out" -eq 3 ]; then
        printf '  ok    %-40s -> exit 3\n' "missing --proof-capsule arg refuses" >&2
    else
        printf '  FAIL  %-40s -> exit %s want 3\n' "missing --proof-capsule arg refuses" "$rc_out" >&2
        failures=$((failures + 1))
    fi

    # Checksum refusal: a mismatched digest must stop before anything executes.
    local blob="$root/archive.tar.xz" rc
    printf 'not an archive' >"$blob"
    ( verify_checksum "$blob" "0000000000000000000000000000000000000000000000000000000000000000" ) 2>/dev/null
    rc=$?
    if [ "$rc" -eq 3 ]; then
        printf '  ok    %-40s -> exit 3\n' "checksum mismatch refuses" >&2
    else
        printf '  FAIL  %-40s -> exit %s want 3\n' "checksum mismatch refuses" "$rc" >&2
        failures=$((failures + 1))
    fi
    ( verify_checksum "$blob" "$(sha256_of "$blob")" ) 2>/dev/null
    rc=$?
    if [ "$rc" -eq 0 ]; then
        printf '  ok    %-40s -> exit 0\n' "checksum match accepted" >&2
    else
        printf '  FAIL  %-40s -> exit %s want 0\n' "checksum match accepted" "$rc" >&2
        failures=$((failures + 1))
    fi

    if [ "$failures" -eq 0 ]; then
        printf 'release_binary_probe self-test: PASS\n' >&2
        exit 0
    fi
    printf 'release_binary_probe self-test: %s case(s) FAILED\n' "$failures" >&2
    exit 2
}

verify_checksum() {
    local file="$1" want="$2" got
    got="$(sha256_of "$file")"
    want="$(printf '%s' "$want" | tr 'A-F' 'a-f')"
    if [ -z "$want" ] || [ "$got" != "$want" ]; then
        printf 'release_binary_probe: checksum mismatch for %s: got %s want %s; refusing to execute it\n' \
            "$file" "$got" "${want:-<none>}" >&2
        exit 3
    fi
}

infer_target_from_archive() {
    local base
    base="$(basename "$1")"
    case "$base" in
        ee-*.tar.xz)
            base="${base#ee-}"
            base="${base%.tar.xz}"
            printf '%s' "$base"
            ;;
        ee-*.tar.gz)
            base="${base#ee-}"
            base="${base%.tar.gz}"
            printf '%s' "$base"
            ;;
    esac
}

verify_release_dir() {
    local dir="$1"
    shift
    local allow_compile_only=" $* "
    [ -d "$dir" ] || die_env "--verify-release-dir directory not found: $dir"
    if [ -n "${PROOF_CAPSULE:-}" ] && [ ! -f "$PROOF_CAPSULE" ]; then
        printf 'release_binary_probe: proof capsule not found: %s\n' "$PROOF_CAPSULE" >&2
        exit 1
    fi
    local found=0 failures=0 capsule_verified=0 archive target probe_file sha got_sha got_verdict got_target got_schema got_checks got_commit
    for archive in "$dir"/ee-*.tar.xz; do
        [ -f "$archive" ] || continue
        found=$((found + 1))
        target="$(infer_target_from_archive "$archive")"
        probe_file="$dir/release-probe-$target.json"
        if [ ! -s "$probe_file" ]; then
            printf 'release_binary_probe: missing release probe capture %s for %s\n' "$probe_file" "$archive" >&2
            failures=$((failures + 1))
            continue
        fi
        if [ -n "$CANDIDATE_SHA" ]; then
            if ! jq -e '[.checks[] | select(.id == "pack_near_tie_determinism")] |
                length == 1 and .[0].kind == "check" and .[0].pass == true' "$probe_file" >/dev/null; then
                printf 'release_binary_probe: %s requires a passing pack_near_tie_determinism check\n' "$probe_file" >&2
                failures=$((failures + 1))
                continue
            fi
            if python3 "$ROOT/scripts/lib/emit_proof_capsule.py" \
                --verify-capsule "$PROOF_CAPSULE" --expected-sha "$CANDIDATE_SHA" --verify-logs \
                --release-archive "$archive" --release-probe "$probe_file" \
                --expected-version "$CANDIDATE_VERSION" >&2; then
                capsule_verified=$((capsule_verified + 1))
            else
                failures=$((failures + 1))
            fi
            continue
        fi
        sha="$(sha256_of "$archive")"
        got_schema="$(jq -r '.schema // empty' "$probe_file" 2>/dev/null)"
        got_sha="$(jq -r '.archive.sha256 // empty' "$probe_file" 2>/dev/null)"
        got_verdict="$(jq -r '.verdict // empty' "$probe_file" 2>/dev/null)"
        got_target="$(jq -r '.binary.targetTriple // empty' "$probe_file" 2>/dev/null)"
        got_commit="$(jq -r '.binary.gitCommit // empty' "$probe_file" 2>/dev/null)"
        got_checks="$(jq -r '(.checks // []) | length' "$probe_file" 2>/dev/null || echo 0)"
        if [ "$got_schema" != "$SCHEMA" ]; then
            printf 'release_binary_probe: %s has schema %s (want %s)\n' "$probe_file" "${got_schema:-<absent>}" "$SCHEMA" >&2
            failures=$((failures + 1))
        fi
        if [ "$got_sha" != "$sha" ]; then
            printf 'release_binary_probe: %s archive.sha256 %s does not match %s (%s)\n' "$probe_file" "${got_sha:-<absent>}" "$archive" "$sha" >&2
            failures=$((failures + 1))
        fi
        if [ "$got_target" != "$target" ]; then
            printf 'release_binary_probe: %s binary.targetTriple %s does not match archive target %s\n' "$probe_file" "${got_target:-<absent>}" "$target" >&2
            failures=$((failures + 1))
        fi
        case "$allow_compile_only" in
            *" $target "*)
                if [ "$got_verdict" != "pass" ] && [ "$got_verdict" != "compile_only" ]; then
                    printf 'release_binary_probe: %s verdict is %s (want pass or compile_only)\n' "$probe_file" "${got_verdict:-<absent>}" >&2
                    failures=$((failures + 1))
                fi
                if [ -n "${PROOF_CAPSULE:-}" ]; then
                    if ! printf '%s' "$got_commit" | grep -Eq '^[0-9a-f]{40}$'; then
                        printf 'release_binary_probe: %s binary.gitCommit %s is not a 40-hex commit SHA required for --proof-capsule\n' "$probe_file" "${got_commit:-<absent>}" >&2
                        failures=$((failures + 1))
                    elif ! python3 "$ROOT/scripts/lib/emit_proof_capsule.py" --verify-capsule "$PROOF_CAPSULE" --expected-sha "$got_commit" >&2; then
                        printf 'release_binary_probe: %s binary.gitCommit %s failed proof capsule verification (%s)\n' "$probe_file" "$got_commit" "$PROOF_CAPSULE" >&2
                        failures=$((failures + 1))
                    else
                        capsule_verified=$((capsule_verified + 1))
                    fi
                fi
                ;;
            *)
                if [ "$got_verdict" != "pass" ] || [ "${got_checks:-0}" -lt 12 ] 2>/dev/null; then
                    printf 'release_binary_probe: %s verdict=%s checks=%s (want pass with >=12 checks)\n' "$probe_file" "${got_verdict:-<absent>}" "${got_checks:-0}" >&2
                    failures=$((failures + 1))
                fi
                if [ -n "${PROOF_CAPSULE:-}" ]; then
                    if ! printf '%s' "$got_commit" | grep -Eq '^[0-9a-f]{40}$'; then
                        printf 'release_binary_probe: %s binary.gitCommit %s is not a 40-hex commit SHA required for --proof-capsule\n' "$probe_file" "${got_commit:-<absent>}" >&2
                        failures=$((failures + 1))
                    elif ! python3 "$ROOT/scripts/lib/emit_proof_capsule.py" --verify-capsule "$PROOF_CAPSULE" --expected-sha "$got_commit" >&2; then
                        printf 'release_binary_probe: %s binary.gitCommit %s failed proof capsule verification (%s)\n' "$probe_file" "$got_commit" "$PROOF_CAPSULE" >&2
                        failures=$((failures + 1))
                    else
                        capsule_verified=$((capsule_verified + 1))
                    fi
                fi
                ;;
        esac
    done
    if [ "$found" -eq 0 ]; then
        printf 'release_binary_probe: no ee-*.tar.xz archives found in %s\n' "$dir" >&2
        exit 1
    fi
    if [ -n "${PROOF_CAPSULE:-}" ] && [ "$capsule_verified" -eq 0 ] && [ "$failures" -eq 0 ]; then
        printf 'release_binary_probe: --proof-capsule was supplied but no archive commit was verified against %s\n' "$PROOF_CAPSULE" >&2
        failures=$((failures + 1))
    fi
    if [ "$failures" -gt 0 ]; then
        printf 'release_binary_probe: %s release-probe capture check(s) failed in %s\n' "$failures" "$dir" >&2
        exit 1
    fi
    printf 'release_binary_probe: verified %s release-probe capture(s) in %s\n' "$found" "$dir" >&2
    exit 0
}

# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------
ARCHIVE="" SHA="" EXPECTED_TARGET="" OUTPUT_FILE="" VERIFY_RELEASE_DIR="" PROOF_CAPSULE=""
CANDIDATE_SHA="" CANDIDATE_VERSION=""
ALLOW_COMPILE_ONLY=() MODEL_CACHE="" WORKDIR="" REPLAY="" JSON=0 SELF_TEST=0
while [ $# -gt 0 ]; do
    case "$1" in
        --archive)
            [ $# -ge 2 ] && [ -n "${2:-}" ] || die_env "--archive requires a non-empty URL or path"
            ARCHIVE="$2"
            shift 2
            ;;
        --sha256)
            [ $# -ge 2 ] && [ -n "${2:-}" ] || die_env "--sha256 requires a non-empty hex digest"
            SHA="$2"
            shift 2
            ;;
        --expected-target)
            [ $# -ge 2 ] && [ -n "${2:-}" ] || die_env "--expected-target requires a non-empty target triple"
            EXPECTED_TARGET="$2"
            shift 2
            ;;
        --output)
            [ $# -ge 2 ] && [ -n "${2:-}" ] || die_env "--output requires a non-empty file path"
            OUTPUT_FILE="$2"
            shift 2
            ;;
        --verify-release-dir)
            [ $# -ge 2 ] && [ -n "${2:-}" ] || die_env "--verify-release-dir requires a non-empty directory path"
            VERIFY_RELEASE_DIR="$2"
            shift 2
            ;;
        --proof-capsule)
            [ $# -ge 2 ] && [ -n "${2:-}" ] || die_env "--proof-capsule requires a non-empty file path"
            PROOF_CAPSULE="$2"
            shift 2
            ;;
        --candidate-sha)
            [ $# -ge 2 ] && [[ "$2" =~ ^[0-9a-f]{40}$ ]] || die_env "--candidate-sha requires a full lowercase commit SHA"
            CANDIDATE_SHA="$2"
            shift 2
            ;;
        --candidate-version)
            [ $# -ge 2 ] && [ -n "${2:-}" ] || die_env "--candidate-version requires the release version"
            CANDIDATE_VERSION="$2"
            shift 2
            ;;
        --allow-compile-only)
            [ $# -ge 2 ] && [ -n "${2:-}" ] || die_env "--allow-compile-only requires a target triple"
            ALLOW_COMPILE_ONLY+=("$2")
            shift 2
            ;;
        --model-cache)
            [ $# -ge 2 ] && [ -n "${2:-}" ] || die_env "--model-cache requires a non-empty directory path"
            MODEL_CACHE="$2"
            shift 2
            ;;
        --workdir)
            [ $# -ge 2 ] && [ -n "${2:-}" ] || die_env "--workdir requires a non-empty directory path"
            WORKDIR="$2"
            shift 2
            ;;
        --replay)
            [ $# -ge 2 ] && [ -n "${2:-}" ] || die_env "--replay requires a non-empty directory path"
            REPLAY="$2"
            shift 2
            ;;
        --json) JSON=1; shift ;;
        --self-test) SELF_TEST=1; shift ;;
        -h | --help) sed -n '2,48p' "$0"; exit 0 ;;
        *) die_env "unknown argument: $1 (see --help)" ;;
    esac
done

[ "$SELF_TEST" = 1 ] && self_test

need jq
if [ -n "$CANDIDATE_SHA" ] || [ -n "$CANDIDATE_VERSION" ]; then
    [ -n "$VERIFY_RELEASE_DIR" ] && [ -n "$PROOF_CAPSULE" ] &&
        [ -n "$CANDIDATE_SHA" ] && [ -n "$CANDIDATE_VERSION" ] ||
        die_env "candidate verification requires --verify-release-dir, --proof-capsule, --candidate-sha and --candidate-version"
    [ -z "${ALLOW_COMPILE_ONLY[*]:-}" ] || die_env "candidate artifact verification requires a native probe for every included archive"
fi
if [ -n "$VERIFY_RELEASE_DIR" ]; then
    verify_release_dir "$VERIFY_RELEASE_DIR" "${ALLOW_COMPILE_ONLY[@]+"${ALLOW_COMPILE_ONLY[@]}"}"
fi
if [ -n "$REPLAY" ]; then
    [ -d "$REPLAY" ] || die_env "--replay directory not found: $REPLAY"
    CAP="$REPLAY"
    grade
    emit "$(verdict_json "replay:$REPLAY" "$(cat "$REPLAY/archive.sha256" 2>/dev/null || echo unknown)")"
fi

[ -n "$ARCHIVE" ] || die_env "--archive <url|path> is required (or --replay / --self-test)"
need tar
need seq
need python3

if [ -z "$WORKDIR" ]; then
    WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/ee-release-probe.XXXXXX")" || die_env "cannot create a work directory"
fi
mkdir -p "$WORKDIR" || die_env "cannot create $WORKDIR"
WORK="$(physical_dir "$WORKDIR")"
CAP="$WORK/capture"
mkdir -p "$CAP" "$WORK/archive" "$WORK/unpacked"
printf 'release_binary_probe workdir: %s\n' "$WORK" >&2

archive_file="$WORK/archive/$(basename "$ARCHIVE")"
case "$ARCHIVE" in
    http://* | https://*)
        need curl
        # AGENTS.md: every web request sets this user agent.
        curl -fsSL -A "$PROBE_USER_AGENT" -o "$archive_file" "$ARCHIVE" || die_env "download failed: $ARCHIVE"
        if [ -z "$SHA" ]; then
            SHA="$(curl -fsSL -A "$PROBE_USER_AGENT" "$ARCHIVE.sha256" | awk '{print $1}')" ||
                die_env "no --sha256 given and $ARCHIVE.sha256 could not be fetched"
        fi
        ;;
    *)
        [ -f "$ARCHIVE" ] || die_env "archive not found: $ARCHIVE"
        cp "$ARCHIVE" "$archive_file"
        if [ -z "$SHA" ]; then
            [ -f "$ARCHIVE.sha256" ] || die_env "no --sha256 given and $ARCHIVE.sha256 does not exist"
            SHA="$(awk '{print $1}' "$ARCHIVE.sha256")"
        fi
        ;;
esac
verify_checksum "$archive_file" "$SHA"
ARCHIVE_SHA="$(sha256_of "$archive_file")"
printf '%s\n' "$ARCHIVE_SHA" >"$CAP/archive.sha256"
if [ -z "$EXPECTED_TARGET" ]; then
    EXPECTED_TARGET="$(infer_target_from_archive "$ARCHIVE")"
fi
if [ -n "$EXPECTED_TARGET" ]; then
    printf '%s\n' "$EXPECTED_TARGET" >"$CAP/expected_target"
fi

tar -xf "$archive_file" -C "$WORK/unpacked" || die_env "could not unpack $archive_file"
EE_BIN="$(command find "$WORK/unpacked" -type f -name ee | head -n 1)"
[ -n "$EE_BIN" ] || die_env "no 'ee' executable inside $archive_file (Windows archives are not supported)"
chmod +x "$EE_BIN"
binary_sha="$(sha256_of "$EE_BIN")" || die_env "cannot hash unpacked executable"
printf '%s\n' "$binary_sha" >"$CAP/binary.sha256" || die_env "cannot record unpacked executable digest"

export HOME="$WORK/home"
export XDG_CONFIG_HOME="$WORK/config" XDG_DATA_HOME="$WORK/data" XDG_CACHE_HOME="$WORK/cache"
export EE_WORKSPACE_REGISTRY="$WORK/workspace-registry.json" EE_EMBED_DOWNLOAD=off EE_NO_COLOR=1
unset EE_EMBED_MODEL_DIR EE_EMBED_MODEL_PATH
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME/ee/models" "$XDG_CACHE_HOME"

if [ -z "$MODEL_CACHE" ]; then
    MODEL_CACHE="$OPERATOR_HOME/.local/share/ee/models"
fi
if [ -d "$MODEL_CACHE/potion-multilingual-128M" ]; then
    # Clone, never link: the probe must not be able to mutate the operator's cache.
    cp -Rc "$MODEL_CACHE/potion-multilingual-128M" "$XDG_DATA_HOME/ee/models/" 2>/dev/null ||
        cp -R --reflink=auto "$MODEL_CACHE/potion-multilingual-128M" "$XDG_DATA_HOME/ee/models/" 2>/dev/null ||
        cp -R "$MODEL_CACHE/potion-multilingual-128M" "$XDG_DATA_HOME/ee/models/" ||
        die_env "could not clone the model cache from $MODEL_CACHE"
else
    printf 'release_binary_probe: no Model2Vec cache at %s; the neural precondition will fail and the verdict will be incomplete\n' \
        "$MODEL_CACHE" >&2
fi

PACK_STAGE_REQUIRED=1
run_probe
grade
emit "$(verdict_json "$ARCHIVE" "$ARCHIVE_SHA")"
