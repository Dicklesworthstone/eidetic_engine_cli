#!/bin/sh
# Closure Linter: forbid abstention-as-implementation closures (EE-ut0q)
#
# Enforces the honesty-only vs implements-surface bead taxonomy:
# - implements-surface:* beads cannot close with abstention language
# - implements-surface:* beads cannot close while *_UNAVAILABLE_CODE exists
# - implements-surface:* beads must have a golden snapshot
# - honesty-only beads must have an open implements-surface sibling, unless a
#   matching implementation sibling is already closed and no sentinel remains
#
# Usage:
#   ./scripts/closure-lint.sh                # Lint relevant bead closures changed recently
#   ./scripts/closure-lint.sh --audit        # Audit all relevant closed beads
#   ./scripts/closure-lint.sh --audit --json # CI/verify mode: audit all and write JSON report
#   ./scripts/closure-lint.sh --json         # Write JSON report for the default recent-closure mode
#
# Exit codes: 0=pass, 1=violations found
# Expired defer-to-v2 closures are reopened with a Beads comment after linting.

set -eu

BEADS_FILE=".beads/issues.jsonl"
BEADS_DIR=".beads"
BEADS_WRITE_LOCK="$BEADS_DIR/.write.lock"
BEADS_SYNC_LOCK="$BEADS_DIR/.sync.lock"
BEADS_LOCK_WAIT_SECONDS="${EE_BEADS_LOCK_WAIT_SECONDS:-30}"
CLI_MOD="src/cli/mod.rs"
REPORT_FILE=".closure-lint-report.json"
GOLDEN_DIR="tests/golden"
SCHEMA_DIR="docs/schemas"
SNAPSHOT_DIR="tests/snapshots"

# Abstention patterns that indicate stub/placeholder closures
DEFER_V2_REGEX='defer.*v2|deferred until v2|v1 honesty stub'
ABSTENTION_REGEX="abstain|unavailable|degraded|stub|placeholder|removed simulation|honest empty|conservative abstention|$DEFER_V2_REGEX"
TODAY_ISO8601="${CLOSURE_LINT_TODAY:-$(date -u +%Y-%m-%d)}"
DEFERRAL_EXPIRED_REASON="deferral expired; honest v1 is now a load-bearing stub"
AUTO_REOPEN_EXPIRED_DEFERRALS="${CLOSURE_LINT_AUTO_REOPEN_EXPIRED:-true}"
EXPIRED_DEFERRALS=""

usage() {
    sed -n '2,11p' "$0" | sed 's/^# //' | sed 's/^#//'
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
    usage
    exit 0
fi

write_skip_report() {
    local reason="$1"
    jq -cn --arg reason "$reason" \
        '{violations:[],count:0,status:"skipped",skipped:true,reason:$reason}' > "$REPORT_FILE"
}

beads_lock_wait_seconds() {
    case "$BEADS_LOCK_WAIT_SECONDS" in
        ''|*[!0-9]*)
            echo "error: EE_BEADS_LOCK_WAIT_SECONDS must be a non-negative integer" >&2
            exit 1
            ;;
        *)
            printf "%s" "$BEADS_LOCK_WAIT_SECONDS"
            ;;
    esac
}

skip_for_beads_lock() {
    local reason="$1"
    write_skip_report "$reason"
    if [ "$JSON_OUTPUT" != true ]; then
        echo "Skipping closure-lint: $reason" >&2
    fi
    exit 0
}

acquire_beads_read_locks() {
    [ -d "$BEADS_DIR" ] || return 0

    if ! command -v flock >/dev/null 2>&1; then
        echo "warning: flock not found; reading Beads files without lock coordination" >&2
        return 0
    fi

    local wait_seconds
    wait_seconds=$(beads_lock_wait_seconds)

    if ! exec 8<>"$BEADS_WRITE_LOCK"; then
        skip_for_beads_lock "could not open $BEADS_WRITE_LOCK"
    fi
    if ! flock -s -w "$wait_seconds" 8; then
        skip_for_beads_lock "$BEADS_WRITE_LOCK is held by another process"
    fi

    if ! exec 9<>"$BEADS_SYNC_LOCK"; then
        skip_for_beads_lock "could not open $BEADS_SYNC_LOCK"
    fi
    if ! flock -s -w "$wait_seconds" 9; then
        skip_for_beads_lock "$BEADS_SYNC_LOCK is held by another process"
    fi
}

release_beads_read_locks() {
    flock -u 8 2>/dev/null || true
    flock -u 9 2>/dev/null || true
    exec 8>&- 2>/dev/null || true
    exec 9>&- 2>/dev/null || true
}

JSON_OUTPUT=false
AUDIT_MODE=false
COMMITS="${CLOSURE_LINT_COMMITS:-1}"
for arg in "$@"; do
    case "$arg" in
        --json) JSON_OUTPUT=true ;;
        --audit) AUDIT_MODE=true ;;
        --commits=*) COMMITS="${arg#--commits=}" ;;
    esac
done

acquire_beads_read_locks

if [ ! -f "$BEADS_FILE" ]; then
    echo "error: $BEADS_FILE not found"
    exit 1
fi

VIOLATIONS=""
VIOLATION_COUNT=0

add_violation() {
    local bead_id="$1"
    local label="$2"
    local surface="$3"
    local reason="$4"

    VIOLATION_COUNT=$((VIOLATION_COUNT + 1))
    local object
    object=$(jq -cn \
        --arg bead "$bead_id" \
        --arg label "$label" \
        --arg surface "$surface" \
        --arg reason "$reason" \
        '{bead:$bead,label:$label,surface:$surface,reason:$reason}')
    VIOLATIONS="${VIOLATIONS}${object}
"

    if [ "$JSON_OUTPUT" = true ]; then
        :
    else
        echo "  x $bead_id [$label] surface=$surface: $reason"
    fi
}

write_report() {
    local status="$1"
    if [ -n "$VIOLATIONS" ]; then
        printf "%s" "$VIOLATIONS" |
            jq -s --arg status "$status" '{violations:.,count:length,status:$status}' > "$REPORT_FILE"
    else
        jq -cn --arg status "$status" '{violations:[],count:0,status:$status}' > "$REPORT_FILE"
    fi
}

implementation_surfaces_for_bead() {
    local bead_id="$1"
    jq -r --arg bead_id "$bead_id" '
        select(.id == $bead_id)
        | [
            ((.labels // [])[]? | select(startswith("implements-surface:")) | sub("^implements-surface:"; "")),
            (try (.title | capture("\\[implements-surface:(?<surface>[^]]+)\\]").surface) catch empty)
          ]
        | unique[]
    ' "$BEADS_FILE" 2>/dev/null || true
}

OPEN_SURFACES_JSON=$(
    jq -sc '
        [
          .[]
          | select(.status != "closed")
          | (
              ((.labels // [])[]? | select(startswith("implements-surface:")) | sub("^implements-surface:"; "")),
              (try (.title | capture("\\[implements-surface:(?<surface>[^]]+)\\]").surface) catch empty)
            )
        ]
        | unique
    ' "$BEADS_FILE" 2>/dev/null || echo '[]'
)

ALL_IMPLEMENTATION_SURFACES_JSON=$(
    jq -sc '
        [
          .[]
          | . as $bead
          | [
              (($bead.labels // [])[]? | select(startswith("implements-surface:")) | sub("^implements-surface:"; "")),
              (try ($bead.title | capture("\\[implements-surface:(?<surface>[^]]+)\\]").surface) catch empty)
            ]
          | unique[]
          | {surface: ., bead: $bead.id, status: $bead.status}
        ]
        | unique_by(.surface, .bead, .status)
    ' "$BEADS_FILE" 2>/dev/null || echo '[]'
)

surface_unavailable_constant() {
    local surface="$1"
    echo "$(echo "$surface" | tr '[:lower:]-' '[:upper:]_')_UNAVAILABLE_CODE"
}

surface_has_unavailable_constant() {
    local surface="$1"
    local constant
    constant=$(surface_unavailable_constant "$surface")
    [ -f "$CLI_MOD" ] && grep -q "$constant" "$CLI_MOD" 2>/dev/null
}

surface_has_open_implementation() {
    local surface="$1"
    printf "%s\n" "$ALL_IMPLEMENTATION_SURFACES_JSON" |
        jq -e --arg surface "$surface" '
            any(.[]; .surface == $surface and .status != "closed")
        ' >/dev/null 2>&1
}

surface_has_closed_implementation() {
    local surface="$1"
    printf "%s\n" "$ALL_IMPLEMENTATION_SURFACES_JSON" |
        jq -e --arg surface "$surface" '
            any(.[]; .surface == $surface and .status == "closed")
        ' >/dev/null 2>&1
}

surface_has_golden_snapshot() {
    local surface="$1"
    local underscored
    underscored=$(echo "$surface" | tr '-' '_')

    [ -f "$GOLDEN_DIR/$surface.snap" ] && return 0
    [ -d "$GOLDEN_DIR/$surface" ] &&
        find "$GOLDEN_DIR/$surface" -type f 2>/dev/null | grep -q . &&
        return 0
    [ -d "tests/fixtures/golden/$surface" ] &&
        find "tests/fixtures/golden/$surface" -type f 2>/dev/null | grep -q . &&
        return 0
    find "$GOLDEN_DIR" "tests/fixtures/golden" -type f \
        \( -name "*$surface*" -o -name "*$underscored*" \) 2>/dev/null |
        grep -q .
}

close_reason_contains_abstention() {
    local close_reason="$1"
    local scrubbed

    if ! echo "$close_reason" | grep -qiE "$ABSTENTION_REGEX"; then
        return 1
    fi

    scrubbed=$(
        printf "%s\n" "$close_reason" |
            sed -E 's/[A-Z0-9_]+_UNAVAILABLE_CODE[[:space:]]+(deleted|removed)//Ig' |
            sed -E 's/(deleted|removed)[[:space:]]+[A-Z0-9_]+_UNAVAILABLE_CODE//Ig' |
            sed -E 's/unavailable stubs removed//Ig' |
            sed -E 's/instead of degraded-mode errors//Ig'
    )

    echo "$scrubbed" | grep -qiE "$ABSTENTION_REGEX"
}

close_reason_contains_defer_v2() {
    local close_reason="$1"
    echo "$close_reason" | grep -qiE "$DEFER_V2_REGEX"
}

bead_parent() {
    local bead_id="$1"
    jq -r --arg bead_id "$bead_id" '
        select(.id == $bead_id)
        | if ((.parent // "") != "") then
            .parent
          else
            ([.dependencies[]? | select((.issue_id // "") == $bead_id and (.type // "") == "parent-child") | .depends_on_id][0] // "")
          end
    ' "$BEADS_FILE" 2>/dev/null | head -n 1
}

bead_closed_date() {
    local bead_id="$1"
    jq -r --arg bead_id "$bead_id" '
        select(.id == $bead_id)
        | (.closed_at // .updated_at // .created_at // "")
        | sub("T.*$"; "")
    ' "$BEADS_FILE" 2>/dev/null | head -n 1
}

iso_date_epoch() {
    local date_value="$1"
    jq -nr --arg date_value "$date_value" '
        try (($date_value + "T00:00:00Z") | fromdateiso8601 | floor) catch empty
    '
}

close_reason_date_count() {
    local close_reason="$1"
    local field="$2"
    printf "%s\n" "$close_reason" |
        sed -nE "s/.*${field}:[[:space:]]*([0-9]{4}-[0-9]{2}-[0-9]{2}).*/\1/p" |
        wc -l |
        tr -d ' '
}

close_reason_date_value() {
    local close_reason="$1"
    local field="$2"
    printf "%s\n" "$close_reason" |
        sed -nE "s/.*${field}:[[:space:]]*([0-9]{4}-[0-9]{2}-[0-9]{2}).*/\1/p" |
        head -n 1
}

surface_has_defer_v2_carveout() {
    local bead_id="$1"
    local surface="$2"
    local parent
    local raw
    local normalized

    parent=$(bead_parent "$bead_id")
    raw=$(printf "%s" "$surface" | tr '[:upper:]' '[:lower:]')
    normalized=$(printf "%s" "$raw" | tr '-' '_')

    jq -e --arg parent "$parent" --arg raw "$raw" --arg normalized "$normalized" '
        def same_parent:
            ($parent == "")
            or ((.parent // "") == $parent)
            or any(.dependencies[]?; ((.depends_on_id // "") == $parent and (.type // "") == "parent-child"));
        select((.status // "") == "closed")
        | select(same_parent)
        | ((.title // "") | ascii_downcase) as $title
        | select(
            ($title | contains("adr:" + $raw + "_v2_design"))
            or ($title | contains("adr:" + $normalized + "_v2_design"))
          )
    ' "$BEADS_FILE" >/dev/null 2>&1 || return 1

    jq -e --arg parent "$parent" --arg raw "$raw" --arg normalized "$normalized" '
        def same_parent:
            ($parent == "")
            or ((.parent // "") == $parent)
            or any(.dependencies[]?; ((.depends_on_id // "") == $parent and (.type // "") == "parent-child"));
        select((.status // "") != "closed")
        | select(same_parent)
        | ((.title // "") | ascii_downcase) as $title
        | [(.labels // [])[]? | ascii_downcase] as $labels
        | select(
            ($title | contains("v2:" + $raw))
            or ($title | contains("v2:" + $normalized))
            or ($labels | index("v2:" + $raw))
            or ($labels | index("v2:" + $normalized))
          )
    ' "$BEADS_FILE" >/dev/null 2>&1
}

record_expired_deferral() {
    local bead_id="$1"
    case "
$EXPIRED_DEFERRALS
" in
        *"
$bead_id
"*) ;;
        *) EXPIRED_DEFERRALS="${EXPIRED_DEFERRALS}${bead_id}
" ;;
    esac
}

validate_defer_deadline() {
    local bead_id="$1"
    local surface="$2"
    local close_reason="$3"
    local start_count="$VIOLATION_COUNT"
    local defer_count
    local renewed_count
    local defer_date
    local renewed_date
    local closure_date
    local base_epoch
    local defer_epoch
    local renewed_epoch
    local today_epoch
    local effective_epoch
    local max_epoch

    defer_epoch=""
    renewed_epoch=""

    defer_count=$(close_reason_date_count "$close_reason" "defer_until_iso8601")
    if [ "$defer_count" -eq 0 ]; then
        add_violation "$bead_id" "defer-to-v2" "$surface" "missing defer_until_iso8601: YYYY-MM-DD"
    elif [ "$defer_count" -gt 1 ]; then
        add_violation "$bead_id" "defer-to-v2" "$surface" "defer_until_iso8601 may appear at most once"
    else
        defer_date=$(close_reason_date_value "$close_reason" "defer_until_iso8601")
        defer_epoch=$(iso_date_epoch "$defer_date")
        closure_date=$(bead_closed_date "$bead_id")
        base_epoch=$(iso_date_epoch "$closure_date")
        if [ -z "$defer_epoch" ]; then
            add_violation "$bead_id" "defer-to-v2" "$surface" "defer_until_iso8601 is not a valid ISO8601 date"
        elif [ -z "$base_epoch" ]; then
            add_violation "$bead_id" "defer-to-v2" "$surface" "closed bead is missing a valid closure date"
        else
            max_epoch=$((base_epoch + 180 * 86400))
            if [ "$defer_epoch" -lt "$base_epoch" ] || [ "$defer_epoch" -gt "$max_epoch" ]; then
                add_violation "$bead_id" "defer-to-v2" "$surface" "defer_until_iso8601 must be within 180 days of closure date"
            fi
        fi
    fi

    renewed_count=$(close_reason_date_count "$close_reason" "defer_renewed_until_iso8601")
    effective_epoch="$defer_epoch"
    if [ "$renewed_count" -gt 1 ]; then
        add_violation "$bead_id" "defer-to-v2" "$surface" "defer_renewed_until_iso8601 may appear at most once"
    elif [ "$renewed_count" -eq 1 ]; then
        if ! printf "%s\n" "$close_reason" | grep -qE 'defer_renewal_reason:[[:space:]]*[^[:space:]]'; then
            add_violation "$bead_id" "defer-to-v2" "$surface" "defer_renewal_reason is required with defer_renewed_until_iso8601"
        fi
        renewed_date=$(close_reason_date_value "$close_reason" "defer_renewed_until_iso8601")
        renewed_epoch=$(iso_date_epoch "$renewed_date")
        if [ -z "$renewed_epoch" ]; then
            add_violation "$bead_id" "defer-to-v2" "$surface" "defer_renewed_until_iso8601 is not a valid ISO8601 date"
        else
            effective_epoch="$renewed_epoch"
        fi
    fi

    today_epoch=$(iso_date_epoch "$TODAY_ISO8601")
    if [ -n "${effective_epoch:-}" ] && [ -n "$today_epoch" ] && [ "$effective_epoch" -lt "$today_epoch" ]; then
        add_violation "$bead_id" "defer-to-v2" "$surface" "$DEFERRAL_EXPIRED_REASON"
        record_expired_deferral "$bead_id"
    fi

    [ "$VIOLATION_COUNT" -eq "$start_count" ]
}

honesty_surfaces_for_bead() {
    local bead_id="$1"
    jq -r --arg bead_id "$bead_id" --argjson known_surfaces "$ALL_IMPLEMENTATION_SURFACES_JSON" '
        select(.id == $bead_id)
        | . as $bead
        | [
            ($known_surfaces | map(.surface) | unique)[] as $surface
            | select(
                (($bead.labels // []) | index($surface))
                or any(($bead.labels // [])[]?; . as $label | $surface | startswith($label + "-"))
              )
            | $surface
          ]
        | unique[]
    ' "$BEADS_FILE" 2>/dev/null || true
}

if [ "$JSON_OUTPUT" != true ]; then
    echo "=== Closure Linter ==="
    echo ""
fi

relevant_closed_bead_ids() {
    jq -r '
        select(.status == "closed")
        | select(
            ((.labels // []) | index("honesty-only"))
            or ((.labels // []) | any(startswith("implements-surface:")))
            or ((.title // "") | test("\\[implements-surface:"))
          )
        | .id
    ' "$BEADS_FILE" 2>/dev/null || true
}

recently_changed_bead_ids() {
    if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        relevant_closed_bead_ids
        return
    fi

    base=$(git rev-parse "HEAD~$COMMITS" 2>/dev/null || git rev-list --max-parents=0 HEAD 2>/dev/null | tail -1 || true)
    if [ -z "$base" ]; then
        relevant_closed_bead_ids
        return
    fi

    git diff --unified=0 "$base" -- "$BEADS_FILE" 2>/dev/null |
        sed -n 's/^+{"id":"\([^"]*\)".*/\1/p' |
        sort -u
}

if [ "$AUDIT_MODE" = true ]; then
    BEAD_IDS=$(relevant_closed_bead_ids)
else
    CHANGED_IDS=$(recently_changed_bead_ids)
    BEAD_IDS=""
    for changed_id in $CHANGED_IDS; do
        if jq -e --arg bead_id "$changed_id" '
            select(.id == $bead_id)
            | select(.status == "closed")
            | select(
                ((.labels // []) | index("honesty-only"))
                or ((.labels // []) | any(startswith("implements-surface:")))
                or ((.title // "") | test("\\[implements-surface:"))
              )
        ' "$BEADS_FILE" >/dev/null 2>&1; then
            BEAD_IDS="${BEAD_IDS}${changed_id}
"
        fi
    done
fi

check_graph_schema_docs() {
    for schema in \
        "ee.insights.v1" \
        "ee.context.pack_dna.v1" \
        "ee.why.causal.v1" \
        "ee.health.structural.v1" \
        "ee.status.skyline.v1" \
        "ee.memory.impact_analysis.v1" \
        "ee.proximity.v1" \
        "ee.why.v1" \
        "ee.context.v1"; do
        if [ ! -f "$SCHEMA_DIR/$schema.json" ]; then
            add_violation "bd-bife.1" "schema-governance" "$schema" "missing $SCHEMA_DIR/$schema.json"
        fi

        snapshot_name=$(printf "%s" "$schema" | tr '.' '_')
        if [ ! -f "$SNAPSHOT_DIR/graph_schemas_v1__${snapshot_name}.snap" ]; then
            add_violation "bd-bife.1" "schema-governance" "$schema" "missing $SNAPSHOT_DIR/graph_schemas_v1__${snapshot_name}.snap"
        fi
    done
}

check_graph_schema_docs

if [ -z "$BEAD_IDS" ] && [ "$VIOLATION_COUNT" -eq 0 ]; then
    if [ "$JSON_OUTPUT" != true ]; then
        if [ "$AUDIT_MODE" = true ]; then
            echo "No closed beads with implements-surface or honesty-only labels found."
        else
            echo "No recently changed closed beads with implements-surface or honesty-only labels found."
        fi
    fi
    if [ "$JSON_OUTPUT" = true ]; then
        write_report "pass"
    fi
    exit 0
fi

# Process each bead by ID
for bead_id in $BEAD_IDS; do
    # Get bead data
    labels=$(jq -r "select(.id == \"$bead_id\") | .labels | join(\",\")" "$BEADS_FILE" 2>/dev/null || echo "")
    close_reason=$(jq -r "select(.id == \"$bead_id\") | .close_reason // \"\"" "$BEADS_FILE" 2>/dev/null || echo "")

    [ -z "$labels" ] && continue

    implementation_surfaces=$(implementation_surfaces_for_bead "$bead_id")
    if [ -n "$implementation_surfaces" ]; then
        # Rule 1: Check for abstention language in close_reason
        if close_reason_contains_abstention "$close_reason"; then
            for surface in $implementation_surfaces; do
                if close_reason_contains_defer_v2 "$close_reason"; then
                    if ! surface_has_defer_v2_carveout "$bead_id" "$surface"; then
                        add_violation "$bead_id" "defer-to-v2" "$surface" "missing sibling adr:${surface}_v2_design ADR and open v2:${surface} bead"
                    fi
                    validate_defer_deadline "$bead_id" "$surface" "$close_reason" || true
                else
                    add_violation "$bead_id" "implements-surface" "$surface" "close_reason contains abstention language"
                fi
            done
        fi

        for surface in $implementation_surfaces; do
            # Rule 2: Check if UNAVAILABLE_CODE constant still exists
            if surface_has_unavailable_constant "$surface"; then
                constant=$(surface_unavailable_constant "$surface")
                add_violation "$bead_id" "implements-surface" "$surface" "${constant} still exists in src/cli/mod.rs"
            fi

            # Rule 3: Implements-surface closures need a public golden snapshot.
            if ! surface_has_golden_snapshot "$surface"; then
                add_violation "$bead_id" "implements-surface" "$surface" "missing $GOLDEN_DIR/$surface.snap"
            fi
        done
    fi

    # Check honesty-only beads
    if echo "$labels" | grep -qE '\bhonesty-only\b'; then
        honesty_surfaces=$(honesty_surfaces_for_bead "$bead_id")
        if [ -z "$honesty_surfaces" ]; then
            add_violation "$bead_id" "honesty-only" "unknown" "no implements-surface sibling matches this bead's surface labels"
        fi

        for surface in $honesty_surfaces; do
            if surface_has_open_implementation "$surface"; then
                continue
            fi
            if surface_has_closed_implementation "$surface" &&
                ! surface_has_unavailable_constant "$surface"; then
                continue
            fi
            add_violation "$bead_id" "honesty-only" "$surface" "missing open or completed implements-surface sibling"
        done
    fi
done

reopen_expired_deferrals() {
    [ -n "$EXPIRED_DEFERRALS" ] || return 0
    [ "$AUTO_REOPEN_EXPIRED_DEFERRALS" = true ] || return 0

    if ! command -v br >/dev/null 2>&1; then
        echo "warning: br not found; expired defer-to-v2 beads were not reopened" >&2
        return 0
    fi

    release_beads_read_locks
    printf "%s" "$EXPIRED_DEFERRALS" |
        sort -u |
        while IFS= read -r expired_bead_id; do
            [ -n "$expired_bead_id" ] || continue
            if ! br reopen "$expired_bead_id" --reason "$DEFERRAL_EXPIRED_REASON" --json >/dev/null 2>&1; then
                echo "warning: failed to reopen expired defer-to-v2 bead $expired_bead_id" >&2
            fi
        done
}

reopen_expired_deferrals

# Output results
if [ "$VIOLATION_COUNT" -gt 0 ]; then
    if [ "$JSON_OUTPUT" != true ]; then
        echo ""
        echo "Found $VIOLATION_COUNT violation(s)"
    fi

    if [ "$JSON_OUTPUT" = true ]; then
        write_report "fail"
        echo "Report written to $REPORT_FILE"
    fi

    exit 1
else
    if [ "$JSON_OUTPUT" != true ]; then
        echo "No violations found."
    fi

    if [ "$JSON_OUTPUT" = true ]; then
        write_report "pass"
    fi

    exit 0
fi
