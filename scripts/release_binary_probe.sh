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
#       [--model-cache <models-dir>] [--workdir <dir>] [--json]
#   scripts/release_binary_probe.sh --replay <capture-dir> [--json]
#   scripts/release_binary_probe.sh --self-test
#
# Output (--json): one `ee.release_probe.v1` object on stdout, keyed by the
# archive SHA-256 and the binary's self-reported gitCommit, with one row per
# check: id, kind, expected, observed, pass.
#
# Exit: 0 every check passed, 1 a check failed, 2 self-test failure,
#       3 environment error or incomplete verdict (checksum, download, missing
#       tool, missing model, neural precondition false).
#
# The work directory is kept and its path printed on stderr; nothing is
# deleted by this script.

set -uo pipefail

SCHEMA="ee.release_probe.v1"
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
}

# --------------------------------------------------------------------------
# Grading. Reads only $CAP. Every check emits exactly one row.
# --------------------------------------------------------------------------
ROWS=""

row() {
    local id="$1" kind="$2" expected="$3" observed="$4" pass="$5"
    local one
    one="$(jq -cn --arg id "$id" --arg kind "$kind" --arg expected "$expected" \
        --arg observed "$observed" --argjson pass "$pass" \
        '{id:$id, kind:$kind, expected:$expected, observed:$observed, pass:$pass}')"
    ROWS="${ROWS}${one}"$'\n'
}

# jq over one captured response; prints nothing when the file is absent or
# not JSON, so an empty world reads as a failure, never as a match.
q() {
    local name="$1" filter="$2"
    [ -s "$CAP/$name.json" ] || return 0
    jq -r "$filter" "$CAP/$name.json" 2>/dev/null || true
}

exit_of() {
    [ -f "$CAP/$1.exit" ] && cat "$CAP/$1.exit" || printf 'missing'
}

ok_response() {
    [ "$(exit_of "$1")" = "0" ] &&
        [ "$(q "$1" '(.schema == "ee.response.v2" and .success == true)')" = "true" ]
}

codes_of() {
    q "$1" '[(.degraded // [])[]?.code, (.data.degraded // [])[]?.code] | unique | join(",")'
}

grade() {
    ROWS=""
    local first_id rule_id backend
    first_id="$(q remember_1 '.data.memoryId // empty')"
    rule_id="$(q rule_add '.data.ruleId // .data.id // empty')"

    # PRECONDITION: the default neural configuration is what was calibrated.
    backend="$(q search '.data.embed_backend // empty')"
    row neural_backend_active precondition "search embed_backend == neural_local" \
        "embed_backend=${backend:-<absent>}" \
        "$([ "$backend" = "neural_local" ] && echo true || echo false)"

    # 1. Walking skeleton.
    local failed_steps="" step
    for step in init remember_1 remember_2 remember_3 remember_4 remember_5 \
        remember_6 remember_7 remember_8 index_rebuild search pack why status \
        index_status doctor; do
        ok_response "$step" || failed_steps="${failed_steps}${step}(exit=$(exit_of "$step")) "
    done
    local top pack_items
    top="$(q search '.data.results[0] | (.docId // .memoryId // .id) // empty')"
    pack_items="$(q pack '(.data.pack.items // []) | length')"
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
    local n sig first_sig="" search_ok=true distinct=0 seen=""
    for n in $(seq 1 "$PARALLEL_SEARCHES"); do
        if ! ok_response "parallel_search_$n"; then search_ok=false; fi
        sig="$(q "parallel_search_$n" '[.data.results[]? | [(.docId // .memoryId // .id), .score]] | tostring')"
        case "$sig" in "" | "[]") search_ok=false ;; esac
        case " $seen " in *" $sig "*) ;; *) seen="$seen $sig"; distinct=$((distinct + 1)) ;; esac
        [ -z "$first_sig" ] && first_sig="$sig"
    done
    [ "$distinct" -eq 1 ] || search_ok=false
    row search_deterministic_parallel check \
        "$PARALLEL_SEARCHES parallel identical searches return one non-empty ordered id+score list" \
        "distinctResultLists=$distinct" "$search_ok"

    # 3. Parallel read-only packs agree on hash and items.
    local pack_ok=true hashes="" h pdistinct=0
    for n in $(seq 1 "$PARALLEL_PACKS"); do
        if ! ok_response "parallel_pack_$n"; then pack_ok=false; fi
        h="$(q "parallel_pack_$n" '(.data.pack.hash // "") + "|" + ([(.data.pack.items // [])[]? | (.memoryId // .id)] | tostring)')"
        case "$h" in "" | "|"* | *"|[]") pack_ok=false ;; esac
        case " $hashes " in *" $h "*) ;; *) hashes="$hashes $h"; pdistinct=$((pdistinct + 1)) ;; esac
    done
    [ "$pdistinct" -eq 1 ] || pack_ok=false
    row pack_deterministic_parallel check \
        "$PARALLEL_PACKS parallel --read-only packs share one non-empty hash and item list" \
        "distinctHashItemSets=$pdistinct" "$pack_ok"

    # 4. Ask answers a direct hit and cites it.
    local abstained cited ask_ok=false
    abstained="$(q ask '.data.abstained | tostring')"
    cited="$(q ask '[.data.citations[]?.memoryId] | join(",")')"
    if ok_response ask && [ "$abstained" = "false" ] && [ -n "$first_id" ] &&
        case ",$cited," in *",$first_id,"*) true ;; *) false ;; esac; then
        ask_ok=true
    fi
    row ask_direct_hit check \
        "ask does not abstain and cites the first remembered memory" \
        "abstained=${abstained:-<absent>} citations=[${cited}]" "$ask_ok"

    # 5. The validated rule is searchable (shipped-good in v0.15.2).
    local rule_top rule_search_ok=false
    rule_top="$(q rule_search '.data.results[0] | (.docId // .memoryId // .id) // empty')"
    if ok_response rule_add && ok_response rule_search && [ -n "$rule_id" ] &&
        [ "$rule_top" = "$rule_id" ]; then
        rule_search_ok=true
    fi
    row rule_searchable check \
        "rule add --source-memory succeeds and the rule is the top search hit" \
        "rule=${rule_id:-<none>} top=${rule_top:-<none>}" "$rule_search_ok"

    # 6. bd-3h6bz: the rule packs beside its source memory.
    local mentions rule_pack_ok=false
    mentions=0
    if [ -n "$rule_id" ] && [ -s "$CAP/rule_pack.json" ]; then
        mentions="$(jq -r --arg r "$rule_id" '[.data.pack | .. | strings | select(. == $r)] | length' \
            "$CAP/rule_pack.json" 2>/dev/null || echo 0)"
    fi
    if ok_response rule_pack && [ -n "$rule_id" ] && [ "${mentions:-0}" -ge 1 ] 2>/dev/null; then
        rule_pack_ok=true
    fi
    row rule_packs_beside_source check \
        "pack for the rule's task contains the rule id (bd-3h6bz)" \
        "rule=${rule_id:-<none>} occurrencesInPack=${mentions:-0} packItems=$(q rule_pack '(.data.pack.items // []) | length')" \
        "$rule_pack_ok"

    # 7. An unrelated query abstains instead of admitting everything.
    local count codes unrelated_ok=false
    count="$(q unrelated '.data.resultCount // (.data.results // [] | length)')"
    codes="$(codes_of unrelated)"
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
    local s_overall d_posture agree=false
    s_overall="$(q status '.data.posture.overall // empty')"
    d_posture="$(q doctor '.data.posture // empty')"
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
    local mm_backend mm_codes mm_ok=false
    mm_backend="$(q model_missing '.data.embed_backend // empty')"
    mm_codes="$(codes_of model_missing)"
    if ok_response model_missing && [ "$mm_backend" = "hash_fallback" ]; then
        case ",$mm_codes," in *",embed_model_unavailable,"*) mm_ok=true ;; esac
    fi
    row model_missing_fallback check \
        "EE_EMBED_MODEL_DIR=/nonexistent yields hash_fallback plus embed_model_unavailable" \
        "embed_backend=${mm_backend:-<absent>} degraded=[${mm_codes}]" "$mm_ok"
}

# Assemble the verdict object from ROWS. Precondition false -> incomplete.
verdict_json() {
    local archive_src="$1" archive_sha="$2"
    printf '%s' "$ROWS" | jq -cs \
        --arg schema "$SCHEMA" --arg src "$archive_src" --arg sha "$archive_sha" \
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
              gitDirty: ($v.source.gitDirty // null),
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
    local verdict_obj="$1"
    if [ "$JSON" = 1 ]; then
        printf '%s\n' "$verdict_obj"
    else
        print_human "$verdict_obj"
    fi
    case "$(printf '%s' "$verdict_obj" | jq -r .verdict)" in
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
    resp() { jq -cn --argjson data "$1" '{schema:"ee.response.v2", success:true, data:$data, degraded:[]}'; }
    for f in init index_rebuild why index_status; do resp '{}' >"$d/$f.json"; done
    for i in 1 2 3 4 5 6 7 8; do resp "{\"memoryId\":\"mem_$i\"}" >"$d/remember_$i.json"; done
    resp "{\"memoryId\":\"$mem\"}" >"$d/remember_1.json"
    resp '{"version":"9.9.9","source":{"gitCommit":"abc","gitTag":"v9.9.9","gitDirty":false},"build":{"targetTriple":"t"}}' >"$d/version.json"
    resp "{\"embed_backend\":\"neural_local\",\"resultCount\":2,\"results\":[{\"docId\":\"$mem\",\"score\":0.5},{\"docId\":\"mem_2\",\"score\":0.4}]}" >"$d/search.json"
    resp '{"pack":{"hash":"blake3:h","items":[{"memoryId":"mem_FIRST"}]}}' >"$d/pack.json"
    for i in $(seq 1 "$PARALLEL_SEARCHES"); do cp "$d/search.json" "$d/parallel_search_$i.json"; done
    for i in $(seq 1 "$PARALLEL_PACKS"); do cp "$d/pack.json" "$d/parallel_pack_$i.json"; done
    resp "{\"abstained\":false,\"citations\":[{\"memoryId\":\"$mem\"}]}" >"$d/ask.json"
    resp '{"resultCount":0,"results":[]}' >"$d/unrelated.json"
    resp '{"memoryId":"mem_FAIL"}' >"$d/failure_memory.json"
    resp "{\"ruleId\":\"$rule\"}" >"$d/rule_add.json"
    resp "{\"results\":[{\"docId\":\"$rule\",\"score\":0.9}]}" >"$d/rule_search.json"
    resp "{\"pack\":{\"hash\":\"blake3:r\",\"items\":[{\"memoryId\":\"mem_FAIL\"},{\"memoryId\":\"$rule\"}]}}" >"$d/rule_pack.json"
    resp '{"posture":{"overall":"ok"}}' >"$d/status.json"
    resp '{"posture":"ok","healthy":true}' >"$d/doctor.json"
    jq -cn '{schema:"ee.response.v2", success:true, data:{embed_backend:"hash_fallback"}, degraded:[{code:"embed_model_unavailable"}]}' >"$d/model_missing.json"
    for f in "$d"/*.json; do printf '0\n' >"${f%.json}.exit"; done
}

# mutate <dir> <file-stem> <jq filter>
mutate() {
    local tmp="$1/$2.json.tmp"
    jq -c "$3" "$1/$2.json" >"$tmp" && mv "$tmp" "$1/$2.json"
}

self_test() {
    need jq
    local root failures=0
    root="$(mktemp -d "${TMPDIR:-/tmp}/ee-release-probe-selftest.XXXXXX")"
    printf 'release_binary_probe self-test workdir: %s\n' "$root" >&2

    expect() {
        # expect <label> <dir> <expected verdict> <expected failed ids csv>
        local label="$1" dir="$2" want_verdict="$3" want_failed="$4" got
        CAP="$dir"
        grade
        got="$(verdict_json fixture 0 | jq -r '.verdict + " " + (.failed | join(","))')"
        if [ "$got" = "$want_verdict $want_failed" ]; then
            printf '  ok    %-40s -> %s\n' "$label" "$got" >&2
        else
            printf '  FAIL  %-40s -> got [%s] want [%s %s]\n' "$label" "$got" "$want_verdict" "$want_failed" >&2
            failures=$((failures + 1))
        fi
    }
    case_dir() {
        local d="$root/$1"
        fixture_ok "$d"
        printf '%s' "$d"
    }

    local d
    d="$(case_dir good)"; expect "all checks pass" "$d" pass ""

    d="$(case_dir empty_world)"; command find "$d" -name '*.json' -exec sh -c ': >"$1"' _ {} \;
    expect "empty captures cannot pass" "$d" incomplete \
        "walking_skeleton,search_deterministic_parallel,pack_deterministic_parallel,ask_direct_hit,rule_searchable,rule_packs_beside_source,unrelated_query_abstains,status_doctor_agree,model_missing_fallback"

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

# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------
ARCHIVE="" SHA="" MODEL_CACHE="" WORKDIR="" REPLAY="" JSON=0 SELF_TEST=0
while [ $# -gt 0 ]; do
    case "$1" in
        --archive) ARCHIVE="${2:-}"; shift 2 ;;
        --sha256) SHA="${2:-}"; shift 2 ;;
        --model-cache) MODEL_CACHE="${2:-}"; shift 2 ;;
        --workdir) WORKDIR="${2:-}"; shift 2 ;;
        --replay) REPLAY="${2:-}"; shift 2 ;;
        --json) JSON=1; shift ;;
        --self-test) SELF_TEST=1; shift ;;
        -h | --help) sed -n '2,45p' "$0"; exit 0 ;;
        *) die_env "unknown argument: $1 (see --help)" ;;
    esac
done

[ "$SELF_TEST" = 1 ] && self_test

need jq
if [ -n "$REPLAY" ]; then
    [ -d "$REPLAY" ] || die_env "--replay directory not found: $REPLAY"
    CAP="$REPLAY"
    grade
    emit "$(verdict_json "replay:$REPLAY" "$(cat "$REPLAY/archive.sha256" 2>/dev/null || echo unknown)")"
fi

[ -n "$ARCHIVE" ] || die_env "--archive <url|path> is required (or --replay / --self-test)"
need tar
need seq

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
        curl -fsSL -o "$archive_file" "$ARCHIVE" || die_env "download failed: $ARCHIVE"
        if [ -z "$SHA" ]; then
            SHA="$(curl -fsSL "$ARCHIVE.sha256" | awk '{print $1}')" ||
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

tar -xf "$archive_file" -C "$WORK/unpacked" || die_env "could not unpack $archive_file"
EE_BIN="$(command find "$WORK/unpacked" -type f -name ee | head -n 1)"
[ -n "$EE_BIN" ] || die_env "no 'ee' executable inside $archive_file (Windows archives are not supported)"
chmod +x "$EE_BIN"

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

run_probe
grade
emit "$(verdict_json "$ARCHIVE" "$ARCHIVE_SHA")"
