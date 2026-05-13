#!/usr/bin/env bash
# J11.5 — focused impact report for failure-mode fixture and degraded-code docs changes.
#
# This runner is intentionally read-only. It maps changed paths to the smallest
# trustworthy J6/K3 verification slice it can identify, and it says when that
# slice is partial or ambiguous instead of pretending focused verification is a
# full catalog run.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURE_DIR="$REPO_ROOT/tests/fixtures/failure_modes"

JSON_OUTPUT=0
CHANGED_PATHS=()

usage() {
    cat <<'USAGE'
usage: scripts/failure_mode_impact.sh --changed <path>... [--json]

Examples:
  scripts/failure_mode_impact.sh --changed tests/fixtures/failure_modes/index_stale.json --json
  scripts/failure_mode_impact.sh --changed docs/degraded_codes.md src/core/search.rs --json
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --json)
            JSON_OUTPUT=1
            shift
            ;;
        --changed)
            shift
            while [ "$#" -gt 0 ] && [ "${1#--}" = "$1" ]; do
                CHANGED_PATHS+=("$1")
                shift
            done
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            CHANGED_PATHS+=("$1")
            shift
            ;;
    esac
done

if ! command -v jq >/dev/null 2>&1; then
    echo "failure_mode_impact.sh: jq is required" >&2
    exit 2
fi

if [ "${#CHANGED_PATHS[@]}" -eq 0 ]; then
    if [ "$JSON_OUTPUT" -eq 1 ]; then
        jq -n '{
            schema: "ee.failure_mode_impact.v1",
            success: false,
            error: {
                code: "missing_changed_paths",
                message: "Pass at least one path with --changed.",
                severity: "low",
                repair: "scripts/failure_mode_impact.sh --changed <path>... --json"
            }
        }'
    else
        usage >&2
    fi
    exit 2
fi

normalize_path() {
    local path="${1:?path required}"
    path="${path#./}"
    case "$path" in
        "$REPO_ROOT"/*) path="${path#"$REPO_ROOT"/}" ;;
    esac
    printf '%s\n' "$path"
}

known_codes() {
    find "$FIXTURE_DIR" -maxdepth 1 -type f -name '*.json' -exec basename {} .json \; |
        LC_ALL=C sort
}

code_is_known() {
    local code="${1:?code required}"
    [ -f "$FIXTURE_DIR/$code.json" ]
}

append_line() {
    local current="${1:-}"
    local line="${2:?line required}"
    if [ -n "$current" ]; then
        printf '%s\n%s\n' "$current" "$line"
    else
        printf '%s\n' "$line"
    fi
}

unique_sorted_lines() {
    LC_ALL=C sort -u | sed '/^$/d'
}

codes_in_file() {
    local path="${1:?path required}"
    local absolute="$REPO_ROOT/$path"
    [ -f "$absolute" ] || return 0
    while IFS= read -r code; do
        if grep -Fq "\"$code\"" "$absolute" 2>/dev/null ||
            grep -Fq "\`$code\`" "$absolute" 2>/dev/null ||
            grep -Fq "$code)" "$absolute" 2>/dev/null; then
            printf '%s\n' "$code"
        fi
    done < <(known_codes)
}

json_array_from_lines() {
    jq -R -s 'split("\n") | map(select(length > 0))'
}

CODES=""
CAVEATS=""
UNMAPPED=""
PARTIAL=0
AMBIGUOUS=0
DOCS_CHANGED=0
TAXONOMY_CHANGED=0
DRIVER_CHANGED=0

for raw_path in "${CHANGED_PATHS[@]}"; do
    path="$(normalize_path "$raw_path")"
    case "$path" in
        tests/fixtures/failure_modes/*.json)
            code="$(basename "$path" .json)"
            if code_is_known "$code"; then
                CODES="$(append_line "$CODES" "$code")"
            else
                AMBIGUOUS=1
                UNMAPPED="$(append_line "$UNMAPPED" "$path")"
                CAVEATS="$(append_line "$CAVEATS" "Fixture path $path does not match a known fixture code in tests/fixtures/failure_modes.")"
            fi
            ;;
        docs/degraded_codes.md)
            DOCS_CHANGED=1
            PARTIAL=1
            CAVEATS="$(append_line "$CAVEATS" "docs/degraded_codes.md is generated from all fixtures; path-only impact cannot prove which sections changed.")"
            ;;
        docs/degraded_code_taxonomy.md)
            DOCS_CHANGED=1
            TAXONOMY_CHANGED=1
            PARTIAL=1
            CAVEATS="$(append_line "$CAVEATS" "docs/degraded_code_taxonomy.md can affect classification for many codes; run taxonomy consistency in addition to focused J6 filters.")"
            ;;
        tests/fixtures/failure_modes/README.md|tests/fixtures/failure_modes/SCHEMA.md)
            DOCS_CHANGED=1
            PARTIAL=1
            CAVEATS="$(append_line "$CAVEATS" "$path describes the catalog as a whole, so focused verification is partial.")"
            ;;
        scripts/e2e_overhaul/failure_modes.sh)
            DRIVER_CHANGED=1
            AMBIGUOUS=1
            CAVEATS="$(append_line "$CAVEATS" "scripts/e2e_overhaul/failure_modes.sh is the catalog driver; changes can affect any fixture scenario.")"
            ;;
        src/*|tests/*|scripts/*)
            matched="$(codes_in_file "$path" | unique_sorted_lines || true)"
            if [ -n "$matched" ]; then
                CODES="$(printf '%s\n%s\n' "$CODES" "$matched")"
                PARTIAL=1
                CAVEATS="$(append_line "$CAVEATS" "$path references mapped fixture code literals, but path-only source analysis is still partial.")"
            else
                AMBIGUOUS=1
                UNMAPPED="$(append_line "$UNMAPPED" "$path")"
                CAVEATS="$(append_line "$CAVEATS" "$path did not contain a known fixture-code literal; full J6 catalog is recommended.")"
            fi
            ;;
        *)
            AMBIGUOUS=1
            UNMAPPED="$(append_line "$UNMAPPED" "$path")"
            CAVEATS="$(append_line "$CAVEATS" "$path is outside the failure-mode mapping rules.")"
            ;;
    esac
done

CODES="$(printf '%s\n' "$CODES" | unique_sorted_lines || true)"
CODES_CSV="$(printf '%s\n' "$CODES" | paste -sd, -)"
CHANGED_JSON="$(printf '%s\n' "${CHANGED_PATHS[@]}" | while IFS= read -r p; do normalize_path "$p"; done | json_array_from_lines)"
CODES_JSON="$(printf '%s\n' "$CODES" | json_array_from_lines)"
CAVEATS_JSON="$(printf '%s\n' "$CAVEATS" | unique_sorted_lines | json_array_from_lines)"
UNMAPPED_JSON="$(printf '%s\n' "$UNMAPPED" | unique_sorted_lines | json_array_from_lines)"

if [ "$AMBIGUOUS" -eq 1 ]; then
    STATUS="ambiguous"
    REASON="At least one changed path cannot be mapped to a complete fixture-code set from path evidence alone."
elif [ "$PARTIAL" -eq 1 ]; then
    STATUS="partial"
    REASON="Changed paths map to fixture codes or catalog docs, but focused verification does not cover the full catalog."
elif [ -n "$CODES" ]; then
    STATUS="complete"
    REASON="All changed paths are concrete failure-mode fixture JSON files with known codes."
else
    STATUS="ambiguous"
    REASON="No fixture codes were identified."
fi

REPORT_JSON="$(
    jq -n \
        --arg status "$STATUS" \
        --arg reason "$REASON" \
        --arg codes_csv "$CODES_CSV" \
        --arg docs_changed "$DOCS_CHANGED" \
        --arg taxonomy_changed "$TAXONOMY_CHANGED" \
        --arg driver_changed "$DRIVER_CHANGED" \
        --argjson changed_paths "$CHANGED_JSON" \
        --argjson fixture_codes "$CODES_JSON" \
        --argjson caveats "$CAVEATS_JSON" \
        --argjson unmapped_paths "$UNMAPPED_JSON" \
        '{
            schema: "ee.failure_mode_impact.v1",
            success: true,
            data: {
                impactStatus: $status,
                reason: $reason,
                changedPaths: $changed_paths,
                fixtureCodes: $fixture_codes,
                unmappedPaths: $unmapped_paths,
                caveats: $caveats,
                signals: {
                    docsChanged: ($docs_changed == "1"),
                    taxonomyChanged: ($taxonomy_changed == "1"),
                    driverChanged: ($driver_changed == "1")
                },
                commands: (
                    [
                        (if ($fixture_codes | length) > 0 then {
                            id: "focused_j6",
                            kind: "e2e",
                            command: ("EE_E2E_KEEP_WORKSPACE=1 EE_FAILURE_MODE_FILTER=" + $codes_csv + " scripts/e2e_overhaul/failure_modes.sh"),
                            covers: "Executable J6 scenarios for mapped fixture codes",
                            coverage: $status
                        } else empty end),
                        {
                            id: "fixture_contract",
                            kind: "cargo-test",
                            command: "cargo test --test contracts failure_mode_fixtures_validate_catalog -- --nocapture",
                            covers: "Failure-mode fixture schema, filename, severity, and source-literal contract",
                            coverage: (if $status == "complete" then "complete" else "partial" end)
                        },
                        {
                            id: "degraded_codes_doc_coverage",
                            kind: "cargo-test",
                            command: "cargo test --test degraded_codes_doc_coverage -- --nocapture",
                            covers: "K3 degraded-code documentation section coverage",
                            coverage: (if ($docs_changed == "1") then "partial" else "supporting" end)
                        }
                    ]
                    + (if ($docs_changed == "1") then [{
                        id: "regenerate_degraded_codes_doc",
                        kind: "manual-regeneration",
                        command: "./scripts/build_degraded_codes_doc.sh",
                        covers: "Regenerates docs/degraded_codes.md from fixture JSON when docs drift is intentional",
                        coverage: "supporting"
                    }] else [] end)
                    + (if ($taxonomy_changed == "1") then [{
                        id: "taxonomy_consistency",
                        kind: "cargo-test",
                        command: "cargo test --test degraded_code_taxonomy_consistency_test -- --nocapture",
                        covers: "Degraded-code taxonomy consistency",
                        coverage: "partial"
                    }] else [] end)
                    + (if ($status != "complete") then [{
                        id: "full_j6_catalog",
                        kind: "e2e",
                        command: "EE_E2E_KEEP_WORKSPACE=1 scripts/e2e_overhaul/failure_modes.sh",
                        covers: "Full executable J6 catalog; recommended because focused impact is not complete",
                        coverage: "complete"
                    }] else [] end)
                ),
                remainingVerification: (
                    if $status == "complete" then
                        ["Focused fixture impact is complete for the supplied path set; full J6 remains the release gate."]
                    elif $status == "partial" then
                        ["Focused commands cover mapped codes and static docs checks only; run full_j6_catalog before claiming full catalog coverage."]
                    else
                        ["Path-only mapping is ambiguous; run full_j6_catalog before closeout."]
                    end
                )
            }
        }'
)"

if [ "$JSON_OUTPUT" -eq 1 ]; then
    printf '%s\n' "$REPORT_JSON"
else
    printf 'failure-mode impact: %s\n' "$STATUS"
    printf 'reason: %s\n' "$REASON"
    if [ -n "$CODES_CSV" ]; then
        printf 'fixture codes: %s\n' "$CODES_CSV"
    fi
    printf 'commands:\n'
    printf '%s\n' "$REPORT_JSON" | jq -r '.data.commands[] | "  - " + .command'
    caveat_count="$(printf '%s\n' "$REPORT_JSON" | jq '.data.caveats | length')"
    if [ "$caveat_count" -gt 0 ]; then
        printf 'caveats:\n'
        printf '%s\n' "$REPORT_JSON" | jq -r '.data.caveats[] | "  - " + .'
    fi
fi
