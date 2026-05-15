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
# The gate warns on ordinary commits when gaps remain. It fails on release-tag
# commits so release assets cannot be cut while documented surfaces are missing
# or only wired to abstention sentinels.

set -eu

README_FILE="README.md"
PLAN_FILE="COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md"
CLI_MOD="src/cli/mod.rs"
BEADS_FILE=".beads/issues.jsonl"
REPORT_FILE=".vision-coverage-report.json"
COMPARE_REF="${VISION_COVERAGE_COMPARE_REF:-}"
SOURCE_REF=""

JSON_OUTPUT=false
FORCE_RELEASE_TAG=false

usage() {
    sed -n '2,13p' "$0" | sed 's/^# //' | sed 's/^#//'
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

normalize_command() {
    raw="$1"
    cleaned=$(
        printf "%s\n" "$raw" |
            sed 's/#.*$//' |
            sed 's/\\$//' |
            sed 's/"[^"]*"//g' |
            sed 's/<[^>]*>//g' |
            sed 's/\[[^]]*\]//g' |
            sed 's/[[:space:]]--.*$//' |
            sed 's/[[:space:]]\+/ /g' |
            sed 's/^ *//' |
            sed 's/ *$//'
    )
    [ -n "$cleaned" ] || return 0

    set -- $cleaned
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
        grep -o '=> "[^"]*"\.to_string()' |
        sed 's/.*=> "//; s/".*//' |
        while IFS= read -r command; do
            canonical_command_alias "$command"
        done |
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

DOCUMENTED_JSON=$(documented_commands | json_array_from_lines)
IMPLEMENTED_JSON=$(implemented_commands | json_array_from_lines)
STUBS_JSON=$(stub_surfaces)
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
        --argjson release_tag "$RELEASE_TAG" '
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
        | (if $gap == 0 then "pass" elif $release_tag then "fail" else "warn" end) as $status
        | {
            schema: "ee.vision_coverage.v1",
            generated_at: $generated_at,
            status: $status,
            release_tag_commit: $release_tag,
            sources: {
              git_ref: (if $source_ref == "" then null else $source_ref end),
              readme: "README.md#Command Reference",
              plan_cli_surface: "COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md#20-cli-surface",
              plan_walking_skeleton: "COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md#29-walking-skeleton",
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
            implemented_surfaces: $implemented_doc,
            missing_surfaces: $missing,
            documented_stubbed_surfaces: $documented_stubbed_surface_records,
            stubbed_surfaces: $stubs,
            documented_surfaces: $doc
          }
    '
}

REPORT_JSON=$(build_report "")

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
else
    echo "=== Vision Coverage Gate ==="
    echo "Documented surfaces: $TOTAL"
    echo "Stubbed surfaces: $STUBBED"
    echo "Missing surfaces: $MISSING"
    echo "Gap: ${GAP}%"
    echo "Report: $REPORT_FILE"
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
        echo "error: vision coverage gap is ${GAP}% on a release-tag commit"
        exit 1
        ;;
    *)
        echo "error: unknown vision coverage status: $STATUS"
        exit 1
        ;;
esac
