#!/bin/sh
# Vision Coverage Gate (eidetic_engine_cli-vwfa)
#
# Compares documented user-facing command surfaces against the actual CLI
# command inventory and the known *_UNAVAILABLE_CODE honesty sentinels.
#
# Usage:
#   sh ./scripts/vision-coverage.sh
#   sh ./scripts/vision-coverage.sh --json
#   sh ./scripts/vision-coverage.sh --release-tag
#
# The gate fails on release-tag commits when ANY gap remains, so release assets
# cannot be cut while documented surfaces are missing or only wired to
# abstention sentinels.
#
# It also fails on ordinary commits once the gap exceeds the cadence threshold
# published in AGENTS.md ("Reality-Check Cadence": gap_percentage > 5). Below
# that threshold an ordinary commit warns and exits 0. Override the threshold
# with VISION_COVERAGE_MAX_GAP_PERCENT.

set -eu

README_FILE="README.md"
PLAN_FILE="COMPREHENSIVE_PLAN.md"
CLI_MOD="src/cli/mod.rs"
BEADS_FILE=".beads/issues.jsonl"
REPORT_FILE=".vision-coverage-report.json"
COMPARE_REF="${VISION_COVERAGE_COMPARE_REF:-}"
# AGENTS.md "Reality-Check Cadence" publishes `gap_percentage > 5` as the point
# at which the reality-check skill must run. Keeping the number here, not only
# in prose, is what lets the gate act on it.
MAX_GAP_PERCENT="${VISION_COVERAGE_MAX_GAP_PERCENT:-5}"
SOURCE_REF=""

# --- behavioral evidence (bd-wn8xh) -----------------------------------------
#
# `surfaces.implemented` counts a documented command as covered when it appears
# in the CLI parser. Parser presence is not behavior: a surface can parse and
# still be exercised by nothing, which is how `ee model status` scored as fully
# covered while reporting a model it did not have (bd-7hsgy).
#
# The behavioral term asks a different question -- does any test or e2e script
# actually INVOKE this surface -- and gates on a shrink-only baseline rather
# than on gap_percentage. That choice is deliberate and measured: the behavioral
# gap on this tree is 4.96%, four hundredths under the 5% cadence threshold, and
# the value moves by whole points with ordinary detector details (7.09% before
# helper-mediated invocations were resolved). A number that volatile must not be
# the thing that decides whether main is red for every agent. A named baseline
# is checkable one entry at a time; a percentage near its own threshold is not.
EVIDENCE_TEST_DIR="tests"
EVIDENCE_SCRIPT_DIR="scripts"
UNEXERCISED_BASELINE_FILE="tests/fixtures/vision_coverage/unexercised_baseline.txt"
NEW_UNEXERCISED_CODE=4
STALE_UNEXERCISED_BASELINE_CODE=5
MISSING_EVIDENCE_CORPUS_CODE=6

JSON_OUTPUT=false
FORCE_RELEASE_TAG=false

usage() {
    sed -n '2,18p' "$0" | sed 's/^# //' | sed 's/^#//'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --help|-h)
            usage
            exit 0
            ;;
        --json)
            JSON_OUTPUT=true
            ;;
        --release-tag)
            FORCE_RELEASE_TAG=true
            ;;
        --compare-ref)
            shift
            if [ "$#" -eq 0 ]; then
                echo "error: --compare-ref requires a git ref"
                exit 1
            fi
            COMPARE_REF="$1"
            ;;
        --compare-ref=*)
            COMPARE_REF="${1#--compare-ref=}"
            ;;
        --report)
            shift
            if [ "$#" -eq 0 ]; then
                echo "error: --report requires a path"
                exit 1
            fi
            REPORT_FILE="$1"
            ;;
        --report=*)
            REPORT_FILE="${1#--report=}"
            ;;
        *)
            echo "error: unknown argument: $1"
            usage
            exit 1
            ;;
    esac
    shift
done

require_file() {
    if [ ! -f "$1" ]; then
        echo "error: required file not found: $1"
        exit 1
    fi
}

require_file "$README_FILE"
require_file "$PLAN_FILE"
require_file "$CLI_MOD"
require_file "$BEADS_FILE"

read_source() {
    if [ -n "$SOURCE_REF" ]; then
        git show "$SOURCE_REF:$1"
    else
        cat "$1"
    fi
}

compare_ref_available() {
    [ -n "$COMPARE_REF" ] || return 1
    git rev-parse --verify "$COMPARE_REF^{commit}" >/dev/null 2>&1 || return 1
    git show "$COMPARE_REF:$README_FILE" >/dev/null 2>&1 || return 1
    git show "$COMPARE_REF:$PLAN_FILE" >/dev/null 2>&1 || return 1
    git show "$COMPARE_REF:$CLI_MOD" >/dev/null 2>&1 || return 1
    git show "$COMPARE_REF:$BEADS_FILE" >/dev/null 2>&1 || return 1
    return 0
}

command_surface() {
    case "$1" in
        audit\ *) echo "audit" ;;
        causal\ *) echo "causal" ;;
        certificate\ *) echo "certificate" ;;
        claim\ *) echo "claim" ;;
        daemon*) echo "daemon" ;;
        demo\ *) echo "demo" ;;
        diag\ quarantine*) echo "diag-quarantine" ;;
        eval\ *) echo "eval" ;;
        handoff\ *) echo "handoff" ;;
        learn\ *) echo "learn" ;;
        maintenance\ *) echo "maintenance-job" ;;
        plan\ *) echo "plan-decisioning" ;;
        preflight\ *) echo "preflight" ;;
        procedure\ *) echo "procedure" ;;
        recorder\ tail*|recorder\ follow*) echo "recorder-tail" ;;
        recorder\ *) echo "recorder-store" ;;
        review\ *) echo "review" ;;
        situation\ *) echo "situation" ;;
        support\ *) echo "support-bundle" ;;
        tripwire\ *) echo "tripwire" ;;
        *)
            printf "%s\n" "$1" | awk '{print $1}'
            ;;
    esac
}

constant_surface() {
    case "$1" in
        AUDIT_UNAVAILABLE_CODE) echo "audit" ;;
        CAUSAL_UNAVAILABLE_CODE) echo "causal" ;;
        CERTIFICATE_STORE_UNAVAILABLE_CODE) echo "certificate" ;;
        CLAIM_UNAVAILABLE_CODE) echo "claim" ;;
        DAEMON_UNAVAILABLE_CODE) echo "daemon" ;;
        DEMO_EXECUTION_UNAVAILABLE_CODE) echo "demo" ;;
        DIAG_QUARANTINE_UNAVAILABLE_CODE) echo "diag-quarantine" ;;
        EVAL_UNAVAILABLE_CODE) echo "eval" ;;
        HANDOFF_UNAVAILABLE_CODE) echo "handoff" ;;
        LEARN_UNAVAILABLE_CODE) echo "learn" ;;
        MAINTENANCE_JOB_UNAVAILABLE_CODE) echo "maintenance-job" ;;
        PLAN_DECISIONING_UNAVAILABLE_CODE) echo "plan-decisioning" ;;
        PREFLIGHT_UNAVAILABLE_CODE) echo "preflight" ;;
        PROCEDURE_UNAVAILABLE_CODE) echo "procedure" ;;
        RECORDER_STORE_UNAVAILABLE_CODE) echo "recorder-store" ;;
        RECORDER_TAIL_UNAVAILABLE_CODE) echo "recorder-tail" ;;
        REVIEW_UNAVAILABLE_CODE) echo "review" ;;
        SITUATION_UNAVAILABLE_CODE) echo "situation" ;;
        SUPPORT_BUNDLE_UNAVAILABLE_CODE) echo "support-bundle" ;;
        TRIPWIRE_STORE_UNAVAILABLE_CODE) echo "tripwire" ;;
        *)
            printf "%s\n" "$1" |
                sed 's/_UNAVAILABLE_CODE$//' |
                tr '[:upper:]_' '[:lower:]-'
            ;;
    esac
}

normalized_command_tokens() {
    saw_command=false
    count=0
    while [ "$#" -gt 0 ] && [ "$count" -lt 3 ]; do
        token="$1"
        shift
        case "$token" in
            --*)
                if [ "$saw_command" = false ]; then
                    option_name="$token"
                    option_has_inline_value=false
                    case "$option_name" in
                        --*=*)
                            option_name="${option_name%%=*}"
                            option_has_inline_value=true
                            ;;
                    esac
                    case "$option_name" in
                        --workspace|--format|--fields|--cards|--schema-version|--shadow|--policy)
                            if [ "$option_has_inline_value" = false ]; then
                                [ "$#" -gt 0 ] && shift
                            fi
                            continue
                            ;;
                        --json|--no-color|--robot|--schema|--legacy-schema|--help-json|--agent-docs|--meta|--experimental-triad|--help|--version)
                            continue
                            ;;
                        *)
                            return 0
                            ;;
                    esac
                fi
                if [ "$#" -gt 0 ]; then
                    case "$1" in
                        -*) ;;
                        *) shift ;;
                    esac
                fi
                continue
                ;;
            -*)
                if [ "$saw_command" = false ]; then
                    case "$token" in
                        -j|-h|-V)
                            continue
                            ;;
                        *)
                            return 0
                            ;;
                    esac
                fi
                continue
                ;;
        esac

        saw_command=true
        count=$((count + 1))
        printf "%s\n" "$token"
    done
}

normalize_command() {
    raw="$1"
    cleaned=$(
        printf "%s\n" "$raw" |
            sed 's/#.*$//' |
            sed 's/\\$//' |
            sed 's/"[^"]*"//g' |
            sed 's/<[^>]*>//g' |
            sed 's/\[[^]]*\]//g' |
            sed 's/[[:space:]]\+/ /g' |
            sed 's/^ *//' |
            sed 's/ *$//'
    )
    [ -n "$cleaned" ] || return 0

    set -- $cleaned
    command_tokens=$(normalized_command_tokens "$@")
    [ -n "$command_tokens" ] || return 0
    set -- $command_tokens
    first="${1:-}"
    second="${2:-}"
    third="${3:-}"

    case "$first" in
        ""|0.1.0|COMMAND|COMMANDS|GLOBAL|OPTIONS|USAGE|ee)
            return 0
            ;;
    esac

    case "$first $second" in
        "learn experiment"|"plan recipe"|"task-frame subgoal"|"outcome quarantine")
            [ -n "$third" ] && printf "%s %s %s\n" "$first" "$second" "$third"
            return 0
            ;;
    esac

    case "$first" in
        agent|analyze|artifact|audit|backup|causal|certificate|claim|curate|demo|diag|economy|eval|focus|graph|handoff|import|index|install|lab|learn|maintenance|memory|mcp|model|plan|playbook|preflight|procedure|recorder|rehearse|review|rule|schema|situation|support|swarm|task-frame|tripwire|workspace|workflow)
            if [ -n "$second" ]; then
                printf "%s %s\n" "$first" "$second"
            else
                printf "%s\n" "$first"
            fi
            ;;
        *)
            printf "%s\n" "$first"
            ;;
    esac
}

canonical_command_alias() {
    case "$1" in
        pack)
            echo "pack build"
            ;;
        graph\ refresh)
            echo "graph centrality-refresh"
            ;;
        *)
            echo "$1"
            ;;
    esac
}

expand_command_inventory() {
    while IFS= read -r command; do
        canonical=$(canonical_command_alias "$command")
        [ -n "$canonical" ] || continue

        prefix=""
        for part in $canonical; do
            if [ -n "$prefix" ]; then
                prefix="$prefix $part"
            else
                prefix="$part"
            fi
            printf "%s\n" "$prefix"
        done
    done
}

extract_readme_command_reference() {
    read_source "$README_FILE" |
        sed -n '/^## Command Reference/,/^## Configuration/p' |
        grep -o '`ee [^`]*`' |
        sed 's/^`ee //; s/`$//'
}

extract_plan_ee_lines() {
    read_source "$PLAN_FILE" |
        sed -n '/^## 20[.] CLI surface/,/^## 21[.] /p' |
        sed -n 's/^[[:space:]]*ee[[:space:]]\+\(.*\)$/\1/p'
    read_source "$PLAN_FILE" |
        sed -n '/^## 29[.] Walking skeleton/,/^## 30[.] /p' |
        sed -n 's/^[[:space:]]*ee[[:space:]]\+\(.*\)$/\1/p'
}

extract_plan_cli_tree() {
    read_source "$PLAN_FILE" |
        sed -n '/^### 20[.]1 Top-level/,/^### 20[.]2 /p' |
        awk '
            BEGIN { in_commands = 0; pending = ""; parent = "" }
            function emit_pending() {
                if (pending != "") {
                    print pending;
                    pending = "";
                }
            }
            /^COMMANDS:/ { in_commands = 1; next }
            /^GLOBAL OPTIONS:/ { emit_pending(); in_commands = 0; next }
            in_commands == 0 { next }
            /^[[:space:]]{4}[[:alnum:]-]+/ {
                line = $0;
                sub(/^[[:space:]]+/, "", line);
                split(line, parts, /[[:space:]]+/);
                name = parts[1];
                if (line ~ / \/ /) {
                    emit_pending();
                    rest = line;
                    sub(/^[^[:space:]]+[[:space:]]+/, "", rest);
                    split(rest, choices, /[[:space:]]*\/[[:space:]]*/);
                    for (i in choices) {
                        split(choices[i], choice_parts, /[[:space:]]+/);
                        if (choice_parts[1] ~ /^[[:alnum:]-]+$/) {
                            print name " " choice_parts[1];
                        }
                    }
                    next;
                }
                emit_pending();
                parent = name;
                pending = name;
                next;
            }
            /^[[:space:]]{8}[[:alnum:]-]+/ {
                line = $0;
                sub(/^[[:space:]]+/, "", line);
                split(line, parts, /[[:space:]]+/);
                if (pending == parent) {
                    pending = "";
                }
                print parent " " parts[1];
                next;
            }
            END { emit_pending() }
        '
}

documented_commands() {
    {
        extract_readme_command_reference
        extract_plan_ee_lines
        extract_plan_cli_tree
    } |
        while IFS= read -r raw; do
            command=$(normalize_command "$raw")
            [ -n "$command" ] || continue
            canonical_command_alias "$command"
        done |
        sed '/^$/d' |
        grep -Ev '^(help|ee|COMMAND|COMMANDS)$' |
        sort -u
}

implemented_commands() {
    read_source "$CLI_MOD" |
        sed -n '/fn extract_command_path(cli: &Cli) -> String {/,/    \/\/\/ Returns a stable identifier suitable/p' |
        grep -o '"[a-z][a-z0-9 -]*"\.to_string()' |
        sed 's/^"//; s/"\.to_string()$//' |
        expand_command_inventory |
        sort -u
}

json_array_from_lines() {
    jq -Rsc 'split("\n") | map(select(length > 0)) | unique'
}

open_implement_surfaces_json() {
    read_source "$BEADS_FILE" |
        jq -Rs '
      split("\n")
      | map(select(length > 0) | fromjson)
      | [
          .[]
          | select(.status != "closed")
          | . as $bead
          | [
              (($bead.labels // [])[]? | select(startswith("implements-surface:")) | sub("^implements-surface:"; "")),
              (try ($bead.title | capture("\\[implements-surface:(?<surface>[^]]+)\\]").surface) catch empty)
            ]
          | unique[]
          | {surface: ., bead: $bead.id}
        ]
      | group_by(.surface)
      | map({surface: .[0].surface, bead: .[0].bead})
    '
}

# How many candidate constants the stub detector's grep can even see.
#
# `stubbed` is one of the two terms in gap_percentage, and stub_surfaces()
# derives it by grepping ONE file ($CLI_MOD) for `const *_UNAVAILABLE_CODE`.
# When that file declares none, `stubbed: 0` is not a measurement of "no
# surfaces are stubbed" -- it is the detector reporting on an empty population,
# and the two are indistinguishable in the published number.
#
# Measured 2026-09-17 (bd-wn8xh): src/cli/mod.rs declares ZERO such constants
# while 41 exist elsewhere under src/, and none of those 41 maps to a documented
# surface. So this term has been structurally pinned at 0, and the gap has been
# carried entirely by `missing`, with nothing in the report saying so.
#
# This does NOT fix the detector -- widening its grep to all of src/ was
# measured to change nothing, because the vocabulary it was written to find
# (the 20-case table in constant_surface) has left the codebase. What it fixes
# is the silence: a zero over an empty population now says it is one.
stub_detector_candidate_count() {
    read_source "$CLI_MOD" |
        { grep -c 'const [A-Z0-9_]*_UNAVAILABLE_CODE' || true; } |
        head -1 |
        tr -d '[:space:]'
}

stub_surfaces() {
    open_json=$(open_implement_surfaces_json)
    read_source "$CLI_MOD" |
        { grep -o 'const [A-Z0-9_]*_UNAVAILABLE_CODE' || true; } |
        awk '{print $2}' |
        sort -u |
        while IFS= read -r constant; do
            surface=$(constant_surface "$constant")
            implements_bead=$(
                printf "%s\n" "$open_json" |
                    jq -r --arg surface "$surface" '
                        first(.[] | select(.surface == $surface) | .bead) // empty
                    '
            )
            if [ -n "$implements_bead" ]; then
                jq -cn \
                    --arg name "$surface" \
                    --arg stub_constant "$constant" \
                    --arg implements_bead "$implements_bead" \
                    '{name:$name, stub_constant:$stub_constant, implements_bead:$implements_bead}'
            else
                jq -cn \
                    --arg name "$surface" \
                    --arg stub_constant "$constant" \
                    '{name:$name, stub_constant:$stub_constant, implements_bead:null}'
            fi
        done |
        jq -s 'sort_by(.name, .stub_constant)'
}

# Shared awk helpers for both evidence extractions.
#
# emit_path() reduces an argument vector to the command it invokes: skip the
# literal binary name, drop global options (and the value of the ones that take
# one), then take the first one or two bare words. It refuses `--help`/`-h`
# outright -- a help probe proves the parser knows the name, which is precisely
# the evidence this term declines to accept.
EVIDENCE_AWK='
    function is_flag(t) { return substr(t, 1, 1) == "-" }
    function is_word(t) { return t ~ /^[a-z][a-z0-9-]*$/ }
    function takes_value(t,   n) {
        n = t
        sub(/=.*$/, "", n)
        if (n != t) return 0
        return (n == "--workspace" || n == "--format" || n == "--fields" ||
                n == "--cards" || n == "--schema-version" || n == "--shadow" ||
                n == "--policy" || n == "--database" || n == "--config" ||
                n == "--profile" || n == "--socket" || n == "--output" ||
                n == "--max-tokens" || n == "--limit")
    }
    function emit_path(n, arr,   i, first, second) {
        first = ""; second = ""
        for (i = 1; i <= n; i++) {
            if (arr[i] == "ee" && first == "") continue
            if (arr[i] == "--help" || arr[i] == "-h") return
            if (is_flag(arr[i])) {
                if (takes_value(arr[i]) && i < n) i++
                continue
            }
            if (!is_word(arr[i])) {
                if (first == "") continue
                else break
            }
            if (first == "") { first = arr[i]; continue }
            second = arr[i]
            break
        }
        if (first == "") return
        # Emit "<command>\t<file that invokes it>". The source is what turns
        # "134 exercised" from an aggregate into a per-surface claim someone can
        # check, which is the whole point of proving behavioral artifacts rather
        # than presence (bd-2mpct.1). SOURCE is set by each caller because the
        # Rust pass buffers a whole file and flushes it after FILENAME has
        # already advanced to the next one.
        if (second != "") print first " " second "\t" SOURCE
        print first "\t" SOURCE
    }
'

evidence_corpus_present() {
    [ -d "$EVIDENCE_TEST_DIR" ] || return 1
    [ -d "$EVIDENCE_SCRIPT_DIR" ] || return 1
    return 0
}

# Shell functions that forward their arguments to the ee binary.
#
# e2e suites do not call the binary directly; each one wraps it, e.g.
# scripts/e2e_ask.sh:86 `run_json() { ... e2e_log_command "$EE_BIN" "$@" ... }`
# and then invokes `run_json "09-ask-direct" --workspace "$WS" --json ask`.
# Matching only a literal `ee ` would score `ask` as never exercised, which is
# false. Find the functions whose BODY reaches the binary, then read their call
# sites -- counting the chokepoint instead of the invocations is how a helper
# hides the population you meant to measure.
ee_wrapper_names() {
    find "$EVIDENCE_SCRIPT_DIR" -name '*.sh' -type f -exec awk '
        /^[a-z_][a-z0-9_]*\(\)[[:space:]]*\{/ {
            fname = $0; sub(/\(\).*/, "", fname); inbody = 1; body = ""; next
        }
        inbody && /^\}/ {
            if (body ~ /\$\{?EE_(BIN|BINARY)\}?/) print fname
            inbody = 0; next
        }
        inbody { body = body "\n" $0 }
    ' {} + 2>/dev/null | sort -u
}

# Every ee command path invoked anywhere in the executed corpus.
#
# Two independent extractions, both invocation-shaped rather than mention-shaped
# -- comments are stripped first, because a sentence naming a command is exactly
# what let twelve orphaned suites pass an invocation audit on Markdown prose
# (bd-q2nq9). `--help` is likewise skipped: a help probe re-tests the parser,
# which is the thing this term exists to stop accepting as coverage.
exercised_commands() {
    evidence_corpus_present || return 0
    wrappers=$(ee_wrapper_names | tr '\n' ' ')
    {
        # Shell: the tail of every invocation, through the binary or a wrapper.
        #
        # Done entirely in one awk pass rather than grep -oE + sed. An
        # alternation over all ~95 discovered wrapper names costs ~12s in BSD
        # grep -o and ~13s in BSD sed, which alone would blow this stage's
        # budget; awk matches the invoker token by lookup instead and costs
        # nothing measurable.
        find "$EVIDENCE_SCRIPT_DIR" -name '*.sh' -type f \
            -exec awk -v WRAPLIST="$wrappers" "$EVIDENCE_AWK"'
                BEGIN {
                    n = split(WRAPLIST, w, " ")
                    for (i = 1; i <= n; i++) wrappers[w[i]] = 1
                    q = sprintf("%c", 39)
                }
                function is_invoker(t) {
                    return (t == "ee" || t == "EEBIN" || (t in wrappers))
                }
                {
                    line = $0
                    sub(/#.*$/, "", line)
                    # The binary is often reached through "$EE_BIN"; normalize it
                    # before quoted strings collapse, or it becomes an opaque @.
                    gsub(/"?\$\{?EE_(BIN|BINARY)\}?"?/, " EEBIN ", line)
                    gsub(/"[^"]*"/, " @ ", line)
                    gsub(q "[^" q "]*" q, " @ ", line)
                    gsub(/\$\{?[A-Za-z_][A-Za-z0-9_]*\}?/, " @ ", line)
                    # `OUT=$(ee_workspace review workspace ...)` welds the
                    # invoker to the assignment, so split the substitution
                    # punctuation before tokenizing.
                    gsub(/[()]/, " ", line)
                    gsub(/[|;&<>]/, " STOP ", line)

                    n = split(line, t, /[[:space:]]+/)
                    for (i = 1; i <= n; i++) {
                        if (!is_invoker(t[i])) continue
                        m = 0
                        for (j = i + 1; j <= n; j++) {
                            if (t[j] == "STOP") break
                            m++; a[m] = t[j]
                        }
                        if (m > 0) { SOURCE = FILENAME; emit_path(m, a) }
                        for (j = 1; j <= m; j++) delete a[j]
                    }
                }
            ' {} + 2>/dev/null

        # Rust: argument vectors, per file so helper prefixes stay file-scoped.
        find "$EVIDENCE_TEST_DIR" -name '*.rs' -type f ! -path "$EVIDENCE_TEST_DIR/fixtures/*" \
            -exec awk "$EVIDENCE_AWK"'
                FNR == 1 && NR > 1 { flush() }
                { line = $0; sub(/\/\/.*$/, "", line); txt = txt " " line; curfile = FILENAME }
                END { flush() }

                function flush(   rest, arr, n, i, j, seg) {
                    if (txt == "") return
                    # curfile, not FILENAME: at the FNR==1 that triggers this
                    # flush, FILENAME is already the NEXT file, so attributing
                    # evidence to it would name the wrong source every time.
                    SOURCE = curfile
                    gsub(/&[A-Za-z_][A-Za-z0-9_]*\[[^]]*\]/, "@", txt)
                    gsub(/&[A-Za-z_][A-Za-z0-9_.]*/, "@", txt)
                    nprefix = 0; delete prefixes
                    nseg = 0; delete segs
                    rest = txt
                    while (match(rest, /\[[^][]*\]/)) {
                        seg = substr(rest, RSTART + 1, RLENGTH - 2)
                        rest = substr(rest, RSTART + RLENGTH)
                        nseg++; segs[nseg] = seg
                        n = tokenize(seg, arr)
                        if (n >= 2 && is_word(arr[n]) && has_global(n, arr)) {
                            nprefix++; prefixes[nprefix] = arr[n]
                        }
                        for (i = 1; i <= n; i++) delete arr[i]
                    }
                    for (i = 1; i <= nseg; i++) {
                        n = tokenize(segs[i], arr)
                        if (n > 0) emit_with_prefixes(n, arr)
                        for (j = 1; j <= n; j++) delete arr[j]
                    }
                    txt = ""
                }

                function tokenize(seg, arr,   n, tok) {
                    n = 0
                    while (match(seg, /"[^"]*"|@/)) {
                        tok = substr(seg, RSTART, RLENGTH)
                        if (tok != "@") tok = substr(tok, 2, length(tok) - 2)
                        seg = substr(seg, RSTART + RLENGTH)
                        n++; arr[n] = tok
                    }
                    return n
                }

                function has_global(n, arr,   i) {
                    for (i = 1; i <= n; i++)
                        if (arr[i] == "--json" || arr[i] == "--workspace") return 1
                    return 0
                }

                function emit_with_prefixes(n, arr,   i, head) {
                    emit_path(n, arr)
                    head = first_word(n, arr)
                    if (head == "") return
                    for (i = 1; i <= nprefix; i++)
                        if (prefixes[i] != head) print prefixes[i] " " head "\t" SOURCE
                }

                function first_word(n, arr,   i) {
                    for (i = 1; i <= n; i++) {
                        if (arr[i] == "ee") continue
                        if (arr[i] == "--help" || arr[i] == "-h") return ""
                        if (is_flag(arr[i])) { if (takes_value(arr[i]) && i < n) i++; continue }
                        if (!is_word(arr[i])) continue
                        return arr[i]
                    }
                    return ""
                }
            ' {} + 2>/dev/null
    } | sed '/^$/d' | sort -u
}

unexercised_commands() {
    evidence_corpus_present || return 0
    comm -23 "$1" "$2"
}

baseline_unexercised_entries() {
    [ -f "$UNEXERCISED_BASELINE_FILE" ] || return 0
    sed -e 's/#.*$//' -e 's/[[:space:]]*$//' "$UNEXERCISED_BASELINE_FILE" |
        sed '/^$/d' |
        sort -u
}

release_tag_commit() {
    if [ "$FORCE_RELEASE_TAG" = true ]; then
        return 0
    fi
    case "${VISION_COVERAGE_RELEASE_TAG:-}" in
        1|true|TRUE|yes|YES)
            return 0
            ;;
    esac
    if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        git tag --points-at HEAD 2>/dev/null |
            grep -Eq '^v[0-9]+[.][0-9]+[.][0-9]+([-.][A-Za-z0-9._-]+)?$'
    else
        return 1
    fi
}

GENERATED_AT=$(date -u '+%Y-%m-%dT%H:%M:%SZ')

if release_tag_commit; then
    RELEASE_TAG=true
else
    RELEASE_TAG=false
fi

build_report() {
    SOURCE_REF="$1"
    jq -n \
        --arg generated_at "$GENERATED_AT" \
        --arg source_ref "$SOURCE_REF" \
        --argjson documented "$(documented_commands | json_array_from_lines)" \
        --argjson implemented "$(implemented_commands | json_array_from_lines)" \
        --argjson stubs "$(stub_surfaces)" \
        --arg stub_detector_file "$CLI_MOD" \
        --argjson stub_detector_candidates "$(stub_detector_candidate_count)" \
        --argjson release_tag "$RELEASE_TAG" \
        --argjson max_gap "$MAX_GAP_PERCENT" '
        def command_surface($cmd):
          if $cmd | startswith("audit ") then "audit"
          elif $cmd | startswith("causal ") then "causal"
          elif $cmd | startswith("certificate ") then "certificate"
          elif $cmd | startswith("claim ") then "claim"
          elif $cmd | startswith("daemon") then "daemon"
          elif $cmd | startswith("demo ") then "demo"
          elif $cmd | startswith("diag quarantine") then "diag-quarantine"
          elif $cmd | startswith("eval ") then "eval"
          elif $cmd | startswith("handoff ") then "handoff"
          elif $cmd | startswith("learn ") then "learn"
          elif $cmd | startswith("maintenance ") then "maintenance-job"
          elif $cmd | startswith("plan ") then "plan-decisioning"
          elif $cmd | startswith("preflight ") then "preflight"
          elif $cmd | startswith("procedure ") then "procedure"
          elif ($cmd | startswith("recorder tail")) or ($cmd | startswith("recorder follow")) then "recorder-tail"
          elif $cmd | startswith("recorder ") then "recorder-store"
          elif $cmd | startswith("review ") then "review"
          elif $cmd | startswith("situation ") then "situation"
          elif $cmd | startswith("support ") then "support-bundle"
          elif $cmd | startswith("tripwire ") then "tripwire"
          else ($cmd | split(" ")[0])
          end;
        def has_stub($surface): any($stubs[]; .name == $surface);
        def stub_for($surface): first($stubs[] | select(.name == $surface));
        def implemented($cmd): any($implemented[]; . == $cmd);
        $documented as $doc
        | [ $doc[] | {command: ., surface: command_surface(.)} ] as $documented_surfaces
        | [ $documented_surfaces[] | select(has_stub(.surface)) | .command ] | unique as $stubbed
        | [ $documented_surfaces[] | select((has_stub(.surface) | not) and (implemented(.command) | not)) | .command ] | unique as $missing
        | [ $documented_surfaces[] | select((has_stub(.surface) | not) and implemented(.command)) | .command ] | unique as $implemented_doc
        | [ $documented_surfaces[] | select(has_stub(.surface)) | .surface ] | unique as $documented_stubbed_unique_surfaces
        | [ $documented_stubbed_unique_surfaces[] | stub_for(.) ] | sort_by(.name) as $documented_stubbed_surface_records
        | ($doc | length) as $total
        | ($stubbed | length) as $stubbed_count
        | ($missing | length) as $missing_count
        | (if $total == 0 then 0 else (((($stubbed_count + $missing_count) * 10000 / $total) | round) / 100) end) as $gap
        | ($gap > $max_gap) as $reality_check_due
        | (if $gap == 0 then "pass"
           elif $release_tag then "fail"
           elif $reality_check_due then "fail"
           else "warn" end) as $status
        | {
            schema: "ee.vision_coverage.v1",
            generated_at: $generated_at,
            status: $status,
            release_tag_commit: $release_tag,
            max_gap_percentage: $max_gap,
            reality_check_due: $reality_check_due,
            sources: {
              git_ref: (if $source_ref == "" then null else $source_ref end),
              readme: "README.md#Command Reference",
              plan_cli_surface: "COMPREHENSIVE_PLAN.md#20-cli-surface",
              plan_walking_skeleton: "COMPREHENSIVE_PLAN.md#29-walking-skeleton",
              cli: "src/cli/mod.rs",
              beads: ".beads/issues.jsonl"
            },
            surfaces: {
              total_documented: $total,
              implemented: ($implemented_doc | length),
              stubbed: $stubbed_count,
              missing: $missing_count,
              with_open_implements_bead: ([ $stubs[] | select(.implements_bead != null) ] | length)
            },
            gap_percentage: $gap,
            # Whether `surfaces.stubbed` is a measurement or a reading taken
            # over an empty population. Both publish 0; only one of them means
            # "no surfaces are stubbed" (bd-wn8xh).
            stub_detector: {
              scanned_file: $stub_detector_file,
              candidate_constants: $stub_detector_candidates,
              population_empty: ($stub_detector_candidates == 0)
            },
            implemented_surfaces: $implemented_doc,
            missing_surfaces: $missing,
            documented_stubbed_surfaces: $documented_stubbed_surface_records,
            stubbed_surfaces: $stubs,
            documented_surfaces: $doc
          }
    '
}

REPORT_JSON=$(build_report "")

# --- behavioral evidence arms (bd-wn8xh) ------------------------------------
#
# Computed against the working tree only. In compare-ref mode the delta below
# still reports gap_percentage, which is a parser-presence measure; the
# behavioral terms are not differenced against a ref because the evidence
# corpus at that ref is not read.
EVIDENCE_WORK_DIR="${TMPDIR:-/tmp}/vision-coverage-evidence.$$"
mkdir -p "$EVIDENCE_WORK_DIR"
trap 'rm -f "$EVIDENCE_WORK_DIR"/documented "$EVIDENCE_WORK_DIR"/exercised "$EVIDENCE_WORK_DIR"/unexercised "$EVIDENCE_WORK_DIR"/baseline; rmdir "$EVIDENCE_WORK_DIR" 2>/dev/null || true' EXIT

if evidence_corpus_present; then
    EVIDENCE_CORPUS_PRESENT=true
    # Reuse the report's documented set rather than re-running
    # documented_commands: that normalizer spawns several seds per README line
    # and costs ~2s, and recomputing it would also let the two answers drift.
    printf "%s\n" "$REPORT_JSON" | jq -r '.documented_surfaces[]' | sort -u \
        > "$EVIDENCE_WORK_DIR/documented"
    exercised_commands > "$EVIDENCE_WORK_DIR/pairs"
    cut -f1 "$EVIDENCE_WORK_DIR/pairs" | sort -u > "$EVIDENCE_WORK_DIR/exercised"
    comm -23 "$EVIDENCE_WORK_DIR/documented" "$EVIDENCE_WORK_DIR/exercised" \
        > "$EVIDENCE_WORK_DIR/unexercised"

    # Per-surface proof artifact: which file actually invokes it, and how many
    # distinct files do. Sorted, so the published witness is deterministic
    # rather than whichever file the scan happened to reach first.
    awk -F'\t' 'NR == FNR { documented[$0] = 1; next }
                ($1 in documented) {
                    if (!(($1 SUBSEP $2) in seen)) {
                        seen[$1 SUBSEP $2] = 1
                        count[$1]++
                        if (!($1 in witness) || $2 < witness[$1]) witness[$1] = $2
                    }
                }
                END { for (cmd in witness) printf "%s\t%s\t%d\n", cmd, witness[cmd], count[cmd] }' \
        "$EVIDENCE_WORK_DIR/documented" "$EVIDENCE_WORK_DIR/pairs" |
        sort > "$EVIDENCE_WORK_DIR/evidence"
    baseline_unexercised_entries > "$EVIDENCE_WORK_DIR/baseline"
    NEW_UNEXERCISED=$(comm -23 "$EVIDENCE_WORK_DIR/unexercised" "$EVIDENCE_WORK_DIR/baseline")
    STALE_BASELINE=$(comm -13 "$EVIDENCE_WORK_DIR/unexercised" "$EVIDENCE_WORK_DIR/baseline")
    EXERCISED_COUNT=$(comm -12 "$EVIDENCE_WORK_DIR/documented" "$EVIDENCE_WORK_DIR/exercised" | wc -l | tr -d '[:space:]')
    UNEXERCISED_COUNT=$(wc -l < "$EVIDENCE_WORK_DIR/unexercised" | tr -d '[:space:]')
else
    # Fail closed. A gate that cannot see its subject must say so, not pass:
    # "the checker could not look" and "the checker found nothing" are the same
    # exit code everywhere this repo has been bitten. VISION_COVERAGE_ALLOW_NO_CORPUS
    # exists so the gate's own fixtures can run against a tree with no tests/ or
    # scripts/ directory, and it has to be set deliberately.
    EVIDENCE_CORPUS_PRESENT=false
    NEW_UNEXERCISED=""
    STALE_BASELINE=""
    EXERCISED_COUNT=0
    UNEXERCISED_COUNT=0
fi

REPORT_JSON=$(
    printf "%s\n" "$REPORT_JSON" |
        jq \
            --argjson corpus_present "$EVIDENCE_CORPUS_PRESENT" \
            --argjson exercised "$EXERCISED_COUNT" \
            --argjson unexercised "$UNEXERCISED_COUNT" \
            --argjson newly "$(printf "%s\n" "$NEW_UNEXERCISED" | json_array_from_lines)" \
            --argjson stale "$(printf "%s\n" "$STALE_BASELINE" | json_array_from_lines)" \
            --argjson evidence "$(
                if [ -f "$EVIDENCE_WORK_DIR/evidence" ]; then
                    jq -Rsc 'split("\n") | map(select(length > 0) | split("\t"))
                             | map({surface: .[0], witness: .[1], sources: (.[2] | tonumber)})' \
                        < "$EVIDENCE_WORK_DIR/evidence"
                else
                    echo '[]'
                fi
            )" \
            --arg baseline_file "$UNEXERCISED_BASELINE_FILE" '
              . + {
                # Whether a documented surface is actually INVOKED by something
                # that runs, as opposed to merely present in the parser. Gated
                # by a shrink-only baseline, not by gap_percentage (bd-wn8xh).
                behavioral_evidence: {
                  corpus_present: $corpus_present,
                  corpus: ["tests/**/*.rs (excluding tests/fixtures/)", "scripts/**/*.sh"],
                  baseline_file: $baseline_file,
                  exercised: $exercised,
                  unexercised: $unexercised,
                  newly_unexercised: $newly,
                  stale_baseline_entries: $stale,
                  # Per-surface proof artifact. `witness` names a file that
                  # actually invokes the surface, so "exercised" is a claim a
                  # reader can check one row at a time instead of a total they
                  # must take on faith (bd-2mpct.1).
                  evidence: $evidence,
                  gap_percentage: (
                    if .surfaces.total_documented == 0 then 0
                    else ((($unexercised * 10000 / .surfaces.total_documented) | round) / 100)
                    end
                  )
                }
              }
            '
)

if compare_ref_available; then
    BASELINE_REPORT_JSON=$(build_report "$COMPARE_REF")
    REPORT_JSON=$(
        jq -n \
            --argjson current "$REPORT_JSON" \
            --argjson baseline "$BASELINE_REPORT_JSON" \
            --arg ref "$COMPARE_REF" '
              $current
              + {
                  delta_vs_main: {
                    available: true,
                    ref: $ref,
                    baseline_gap_percentage: $baseline.gap_percentage,
                    current_gap_percentage: $current.gap_percentage,
                    gap_delta_percentage: (($current.gap_percentage - $baseline.gap_percentage) * 100 | round / 100),
                    baseline_surfaces: $baseline.surfaces,
                    current_surfaces: $current.surfaces
                  }
                }
            '
    )
elif [ -n "$COMPARE_REF" ]; then
    REPORT_JSON=$(
        jq -n \
            --argjson current "$REPORT_JSON" \
            --arg ref "$COMPARE_REF" '
              $current
              + {
                  delta_vs_main: {
                    available: false,
                    ref: $ref,
                    reason: "compare_ref_unavailable",
                    baseline_gap_percentage: null,
                    current_gap_percentage: $current.gap_percentage,
                    gap_delta_percentage: null,
                    baseline_surfaces: null,
                    current_surfaces: $current.surfaces
                  }
                }
            '
    )
else
    REPORT_JSON=$(printf "%s\n" "$REPORT_JSON" | jq '. + {delta_vs_main: null}')
fi

printf "%s\n" "$REPORT_JSON" > "$REPORT_FILE"

STATUS=$(printf "%s\n" "$REPORT_JSON" | jq -r '.status')
GAP=$(printf "%s\n" "$REPORT_JSON" | jq -r '.gap_percentage')
TOTAL=$(printf "%s\n" "$REPORT_JSON" | jq -r '.surfaces.total_documented')
STUBBED=$(printf "%s\n" "$REPORT_JSON" | jq -r '.surfaces.stubbed')
MISSING=$(printf "%s\n" "$REPORT_JSON" | jq -r '.surfaces.missing')

if [ "$JSON_OUTPUT" = true ]; then
    echo "Report written to $REPORT_FILE"
    # THE DENOMINATOR BESIDE THE COUNTS, because "Report written" is not a
    # verdict. CI invokes this script with --json, so until this line existed
    # the run that matters printed one sentence naming a filename and nothing
    # about what was examined -- indistinguishable, from the log, from a run
    # that examined nothing.
    echo "vision-coverage: $TOTAL documented surfaces examined; stubbed $STUBBED, missing $MISSING, gap ${GAP}%"
    if [ "$(printf "%s\n" "$REPORT_JSON" | jq -r '.stub_detector.population_empty')" = "true" ]; then
        # The same sentence the human branch already carries. It was written
        # for exactly this reading and was suppressed in the only invocation
        # CI uses; moving it here costs nothing and is the difference between
        # a structural zero and a clean bill of health (bd-wn8xh).
        echo "  Stubbed: 0 — NOT A MEASUREMENT: $(printf "%s\n" "$REPORT_JSON" | jq -r '.stub_detector.scanned_file') declares no *_UNAVAILABLE_CODE constants,"
        echo "         so the stub half of the gap is reporting on an empty population, not on an absence of stubs."
    fi
else
    echo "=== Vision Coverage Gate ==="
    echo "Documented surfaces: $TOTAL"
    echo "Stubbed surfaces: $STUBBED"
    echo "Missing surfaces: $MISSING"
    echo "Gap: ${GAP}%"
    if [ "$(printf "%s\n" "$REPORT_JSON" | jq -r '.stub_detector.population_empty')" = "true" ]; then
        # Say it out loud rather than letting a structural zero read as a clean
        # bill of health. `stubbed` is half of gap_percentage (bd-wn8xh).
        echo "Stubbed: 0 — NOT A MEASUREMENT: $(printf "%s\n" "$REPORT_JSON" | jq -r '.stub_detector.scanned_file') declares no *_UNAVAILABLE_CODE constants,"
        echo "         so the stub half of the gap is reporting on an empty population, not on an absence of stubs."
    fi
    if [ "$EVIDENCE_CORPUS_PRESENT" = true ]; then
        BEHAVIORAL_GAP=$(printf "%s\n" "$REPORT_JSON" | jq -r '.behavioral_evidence.gap_percentage')
        echo "Exercised surfaces: $EXERCISED_COUNT of $TOTAL (behavioral gap ${BEHAVIORAL_GAP}%)"
        echo "  Implemented: $TOTAL — parser presence, NOT behavior. A surface counted here"
        echo "  can still be invoked by nothing; the exercised count above is the behavioral one."
        if [ "$UNEXERCISED_COUNT" != 0 ]; then
            echo "  Unexercised (all baselined in $UNEXERCISED_BASELINE_FILE):"
            sed 's/^/    /' "$EVIDENCE_WORK_DIR/unexercised"
        fi
    else
        echo "Exercised surfaces: NOT MEASURED — no $EVIDENCE_TEST_DIR/ or $EVIDENCE_SCRIPT_DIR/ corpus."
    fi
    echo "Report: $REPORT_FILE"
fi

# The behavioral arms are consulted BEFORE the gap ladder below. The ladder's
# first rung is `gap == 0 -> pass`, and on this tree the gap is structurally 0,
# so anything checked after it is unreachable. That ordering is exactly why the
# stub term sat dead for months without the report ever saying so.
if [ "$EVIDENCE_CORPUS_PRESENT" != true ]; then
    case "${VISION_COVERAGE_ALLOW_NO_CORPUS:-}" in
        1|true|TRUE|yes|YES) ;;
        *)
            echo "error: no evidence corpus: $EVIDENCE_TEST_DIR/ or $EVIDENCE_SCRIPT_DIR/ is missing," >&2
            echo "       so the behavioral-evidence term could not be measured at all." >&2
            echo "hint: set VISION_COVERAGE_ALLOW_NO_CORPUS=1 only for fixture trees that" >&2
            echo "      deliberately have no tests/ or scripts/ directory." >&2
            exit "$MISSING_EVIDENCE_CORPUS_CODE"
            ;;
    esac
elif [ -n "$NEW_UNEXERCISED" ] || [ -n "$STALE_BASELINE" ]; then
    if [ -n "$NEW_UNEXERCISED" ]; then
        echo "error: documented surfaces that no executed test or e2e script invokes," >&2
        echo "       and that are not recorded in $UNEXERCISED_BASELINE_FILE:" >&2
        printf "%s\n" "$NEW_UNEXERCISED" | sed 's/^/         /' >&2
        echo "hint: add a test or e2e script that actually invokes the surface. Recording" >&2
        echo "      it in the baseline is for surfaces you are deliberately leaving unproven." >&2
    fi
    if [ -n "$STALE_BASELINE" ]; then
        echo "error: $UNEXERCISED_BASELINE_FILE lists surfaces that ARE now exercised." >&2
        echo "       The list may only shrink; delete these lines:" >&2
        printf "%s\n" "$STALE_BASELINE" | sed 's/^/         /' >&2
    fi
    if [ -n "$NEW_UNEXERCISED" ]; then
        exit "$NEW_UNEXERCISED_CODE"
    fi
    exit "$STALE_UNEXERCISED_BASELINE_CODE"
fi

case "$STATUS" in
    pass)
        exit 0
        ;;
    warn)
        if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
            DELTA=$(printf "%s\n" "$REPORT_JSON" | jq -r '.delta_vs_main.gap_delta_percentage // empty')
            if [ -n "$DELTA" ]; then
                echo "::notice title=Vision coverage delta::gap changed by ${DELTA} percentage point(s) vs ${COMPARE_REF}; current ${GAP}%; see $REPORT_FILE"
            fi
            echo "::warning title=Vision coverage gap::${GAP}% of documented surfaces are missing or stubbed; see $REPORT_FILE"
        else
            echo "warning: vision coverage gap is ${GAP}% (non-release commit)"
        fi
        exit 0
        ;;
    fail)
        if [ "$RELEASE_TAG" = true ]; then
            echo "error: vision coverage gap is ${GAP}% on a release-tag commit"
        else
            MAX_GAP=$(printf "%s\n" "$REPORT_JSON" | jq -r '.max_gap_percentage')
            echo "error: vision coverage gap is ${GAP}%, above the ${MAX_GAP}% reality-check cadence threshold in AGENTS.md"
            echo "hint: run the reality-check-for-project skill end-to-end, or raise VISION_COVERAGE_MAX_GAP_PERCENT deliberately"
        fi
        exit 1
        ;;
    *)
        echo "error: unknown vision coverage status: $STATUS"
        exit 1
        ;;
esac
