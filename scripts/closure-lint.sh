#!/usr/bin/env bash
# Closure Linter: forbid abstention-as-implementation closures (EE-ut0q)
#
# Enforces the honesty-only vs implements-surface bead taxonomy:
# - implements-surface:* beads cannot close with abstention language
# - implements-surface:* beads cannot close while *_UNAVAILABLE_CODE exists
# - implements-surface:* beads must have a golden snapshot
# - math-ambition beads cannot close without explicit rejection-threshold evidence
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
QUALITY_REPORT_FILE=".closure-quality-report.json"
GOLDEN_DIR="tests/golden"
SCHEMA_DIR="docs/schemas"
SNAPSHOT_DIR="tests/snapshots"
TEST_TRACING_HELPER="tests/support/test_tracing.rs"
TEST_TRACING_LOG_DIR="tests/golden/logs"
FAILURE_MODE_FIXTURE_DIR="tests/fixtures/failure_modes"
MATH_AMBITION_REJECTION_LOG="docs/spikes/math_ambition_rejection_log.md"

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

add_failure_mode_fixture_violation() {
    local bead_id="$1"
    local surface="$2"
    local fixture_path="$3"
    local emitted_code="$4"
    local severity="$5"
    local reason="$6"

    VIOLATION_COUNT=$((VIOLATION_COUNT + 1))
    local object
    object=$(jq -cn \
        --arg bead "$bead_id" \
        --arg label "implements-surface" \
        --arg surface "$surface" \
        --arg reason "$reason" \
        --arg bead_id "$bead_id" \
        --arg missing_fixture_path "$fixture_path" \
        --arg emitted_code "$emitted_code" \
        --arg severity "$severity" \
        '{
            bead:$bead,
            label:$label,
            surface:$surface,
            reason:$reason,
            bead_id:$bead_id,
            missing_fixture_path:$missing_fixture_path,
            emitted_code:$emitted_code,
            severity:$severity
        }')
    VIOLATIONS="${VIOLATIONS}${object}
"

    if [ "$JSON_OUTPUT" = true ]; then
        :
    else
        echo "  x $bead_id [implements-surface] surface=$surface: $reason"
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

write_closure_quality_report() {
    local generated_at
    generated_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    jq -s \
        --arg generated_at "$generated_at" \
        --argjson premature_days 14 \
        --argjson trend_days 90 '
        def normalized_epoch($value):
            ($value // "")
            | sub("\\.[0-9]+Z$"; "Z")
            | try fromdateiso8601 catch null;

        ($generated_at | fromdateiso8601) as $generated_epoch
        | [ .[] | select((.status // "") == "closed") ] as $closed_beads
        | [
            $closed_beads[]
            | . as $bead
            | normalized_epoch($bead.closed_at // $bead.updated_at // "") as $closed_epoch
            | normalized_epoch($bead.created_at // "") as $created_epoch
            | select($closed_epoch != null)
            | [
                ($bead.comments // [])[]?
                | select((.text // "") | test("^Reopened:"; "i"))
                | normalized_epoch(.created_at // "") as $reopened_epoch
                | (
                    if $reopened_epoch >= $closed_epoch then
                        {epoch: $closed_epoch, label: "closed_at"}
                    else
                        {epoch: ($created_epoch // $closed_epoch), label: "created_at_fallback"}
                    end
                  ) as $basis
                | select($reopened_epoch != null and $basis.epoch != null and $reopened_epoch >= $basis.epoch)
                | {
                    comment_id: (.id // null),
                    reopened_at: (.created_at // ""),
                    time_basis: $basis.label,
                    reason: (.text // ""),
                    time_to_reopen_days: ((($reopened_epoch - $basis.epoch) / 86400) | floor)
                  }
              ]
            | sort_by(.time_to_reopen_days, .reopened_at)
            | .[0]? as $reopen
            | select($reopen != null)
            | {
                bead_id: $bead.id,
                title: ($bead.title // ""),
                closed_at: ($bead.closed_at // ""),
                reopened_at: $reopen.reopened_at,
                time_to_reopen_days: $reopen.time_to_reopen_days,
                quality_signal: (
                    if $reopen.time_to_reopen_days <= $premature_days then
                        "premature_closure"
                    else
                        "reopened_after_window"
                    end
                ),
                time_basis: $reopen.time_basis,
                reopen_comment_id: $reopen.comment_id,
                reopen_reason: $reopen.reason
              }
          ] as $signals
        | ($signals | map(select(.quality_signal == "premature_closure"))) as $premature
        | (
            $premature
            | map(select((normalized_epoch(.reopened_at) // 0) >= ($generated_epoch - ($trend_days * 86400))))
          ) as $recent_premature
        | {
            schema: "ee.closure_quality_report.v1",
            generatedAt: $generated_at,
            advisory: true,
            thresholds: {
                prematureClosureDays: $premature_days,
                trendWindowDays: $trend_days
            },
            summary: {
                closedBeadsAudited: ($closed_beads | length),
                reopenedBeads: ($signals | length),
                prematureClosures: ($premature | length),
                recentPrematureClosures: ($recent_premature | length)
            },
            trend: {
                window: "last_quarter",
                windowDays: $trend_days,
                prematureClosures: ($recent_premature | length)
            },
            signals: $signals
        }
    ' "$BEADS_FILE" > "$QUALITY_REPORT_FILE"
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

bead_is_bd3usjw_family() {
    local bead_id="$1"
    local parent

    case "$bead_id" in
        bd-3usjw|bd-3usjw.*) return 0 ;;
    esac

    parent=$(bead_parent "$bead_id")
    [ "$parent" = "bd-3usjw" ]
}

bead_declared_rust_file_surfaces() {
    local bead_id="$1"
    jq -r --arg bead_id "$bead_id" '
        select(.id == $bead_id)
        | (.description // "")
    ' "$BEADS_FILE" 2>/dev/null |
        sed -n 's/^FILE SURFACE:[[:space:]]*//p' |
        tr ',' '\n' |
        sed -E 's/^[[:space:]]*//; s/[[:space:]].*$//; s/^`//; s/`$//' |
        grep -E '^tests/.*\.rs$' || true
}

bead_declared_file_surfaces() {
    local bead_id="$1"
    jq -r --arg bead_id "$bead_id" '
        select(.id == $bead_id)
        | (.description // "")
    ' "$BEADS_FILE" 2>/dev/null |
        sed -n 's/^FILE SURFACE:[[:space:]]*//p' |
        tr ',' '\n' |
        sed -E 's/^[[:space:]]*//; s/[[:space:]].*$//; s/^`//; s/`$//' |
        grep -E '^[A-Za-z0-9_./*?+-]+$' || true
}

bead_referenced_test_paths() {
    local bead_id="$1"

    jq -r --arg bead_id "$bead_id" '
        select(.id == $bead_id)
        | [(.description // ""), (.notes // "")] | join("\n")
    ' "$BEADS_FILE" 2>/dev/null |
        tr '`"'"'"'[]{}<>' '\n' |
        grep -Eo 'tests/[A-Za-z0-9_./*?+-]+' |
        sed -E 's/[).,:;]+$//; s#/$##' |
        sort -u || true
}

test_reference_matches() {
    local reference="$1"
    local match

    case "$reference" in
        *'*'*|*'?'*)
            for match in $reference; do
                [ "$match" != "$reference" ] || continue
                [ -e "$match" ] && printf '%s\n' "$match"
            done
            ;;
        *)
            [ -e "$reference" ] && printf '%s\n' "$reference"
            ;;
    esac
}

rust_test_file_has_assertion() {
    local test_file="$1"

    grep -Eq 'assert(!|_eq!|_ne!|_matches!|_json_snapshot!)|prop_assert|proptest!|expect\(|ensure\(|ensure_eq\(' "$test_file"
}

rust_test_file_has_only_ignored_tests() {
    local test_file="$1"
    local test_count
    local ignore_count

    test_count=$(grep -c '#\[test\]' "$test_file" || true)
    ignore_count=$(grep -c '#\[ignore\]' "$test_file" || true)
    [ "$test_count" -gt 0 ] && [ "$ignore_count" -ge "$test_count" ]
}

# bd-3usjw.62 — unit_test_obligation_part_ii helpers.
#
# Returns lines from a bead's FILE SURFACE that point at src/...rs
# implementation files (i.e. the production code paths the bead introduces
# or revises). Used to assert AGENTS.md L300-302 "Every module includes
# inline #[cfg(test)] unit tests alongside the implementation."
bead_declared_src_file_surfaces() {
    local bead_id="$1"
    bead_declared_file_surfaces "$bead_id" |
        grep -E '^src/.*\.rs$' || true
}

# A .rs file satisfies the inline-tests-module shape when it has both a
# `#[cfg(test)]` attribute AND a `mod tests` declaration. The grep checks
# are independent (not adjacency-aware) on purpose — adjacency parsing in
# pure shell is fragile, and the false-accept rate here is tiny in
# practice given the existing module layout in this repo.
rust_src_file_has_inline_tests_module() {
    local src_file="$1"
    grep -q '^[[:space:]]*#\[cfg(test)\]' "$src_file" 2>/dev/null &&
        grep -qE '^[[:space:]]*mod[[:space:]]+tests[[:space:]]*\{' "$src_file" 2>/dev/null
}

rust_src_file_inline_test_count() {
    local src_file="$1"
    grep -c '^[[:space:]]*#\[test\]' "$src_file" 2>/dev/null || true
}

rust_src_file_inline_ignore_count() {
    local src_file="$1"
    grep -c '^[[:space:]]*#\[ignore\]' "$src_file" 2>/dev/null || true
}

# Returns 0 (true) when the file ships >= 3 `#[test]` fns whose count
# exceeds the count of adjacent `#[ignore]` attributes (so a fully-ignored
# test module is rejected the same way `rust_test_file_has_only_ignored_tests`
# rejects the test-side variant).
rust_src_file_has_sufficient_inline_tests() {
    local src_file="$1"
    local count
    local ignore_count

    count=$(rust_src_file_inline_test_count "$src_file")
    [ "$count" -ge 3 ] || return 1
    ignore_count=$(rust_src_file_inline_ignore_count "$src_file")
    [ "$count" -gt "$ignore_count" ]
}

# Rule 7: AGENTS.md L300-302 requires every module to ship inline
# #[cfg(test)] tests covering happy, edge, and error paths. For any
# implements-surface:* bead closing, when its FILE SURFACE declares one
# or more src/...rs implementation files, AT LEAST ONE of those files
# must carry an inline `#[cfg(test)] mod tests` block with >=3 non-ignored
# `#[test]` functions AND assertion-style coverage (per the existing
# `rust_test_file_has_assertion` definition shared with Rule 5).
#
# Beads whose FILE SURFACE contains no src/...rs paths (script-only,
# fixture-only, doc-only surfaces) are exempt: the obligation is for
# implementation modules, not for closure-lint-internal scripts or
# documentation drift gates.
check_implementation_unit_test_obligation() {
    local bead_id="$1"
    local surface="$2"
    local src_file
    local any_existing_src=false
    local satisfied=false
    local first_path=""

    for src_file in $(bead_declared_src_file_surfaces "$bead_id"); do
        # Only enforce against src files that actually exist on disk. A
        # declared-but-missing src path is a different policy concern
        # (file-not-shipped) and would produce a misleading "no inline
        # tests" message; let the existing test-reference/golden gates
        # cover that case.
        [ -f "$src_file" ] || continue
        any_existing_src=true
        if [ -z "$first_path" ]; then
            first_path="$src_file"
        fi
        rust_src_file_has_inline_tests_module "$src_file" || continue
        rust_src_file_has_sufficient_inline_tests "$src_file" || continue
        rust_test_file_has_assertion "$src_file" || continue
        satisfied=true
        break
    done

    [ "$any_existing_src" = false ] && return 0
    [ "$satisfied" = true ] && return 0

    add_violation "$bead_id" "implements-surface" "$surface" \
        "no src/ file in FILE SURFACE has #[cfg(test)] mod tests with >=3 non-ignored #[test] fns and assertion coverage (first declared src path: ${first_path:-<none>})"
}

# bd-3usjw.61 — audit_row_obligation_part_ii helpers.
#
# Beads whose surface mutates durable state must document their audit-row
# emission contract: README L181-193 promises "no silent memory mutation".
# This rule mirrors Rule 7's "validate-when-declared" semantics: if a bead
# description carries an `AUDIT EMISSION:` block, its required fields and
# referenced audit test file must all be present. Beads without an
# `AUDIT EMISSION:` block are out of scope for this rule (the bulk
# retrofit + the durable_write-classification gate are separate
# follow-up beads).

# Returns 0 (true) when the bead description contains an explicit
# `AUDIT EMISSION:` block introducer at the start of a line. Matched
# leniently with leading whitespace allowed.
bead_declares_audit_emission_block() {
    local bead_id="$1"
    jq -r --arg bead_id "$bead_id" '
        select(.id == $bead_id)
        | (.description // "")
    ' "$BEADS_FILE" 2>/dev/null |
        grep -qE '^[[:space:]]*AUDIT EMISSION:'
}

# Whole-description text for the bead (decoded). Reused below for each
# component check so we walk the JSONL once per assertion rather than
# re-shelling per token.
bead_description_text() {
    local bead_id="$1"
    jq -r --arg bead_id "$bead_id" '
        select(.id == $bead_id)
        | (.description // "")
    ' "$BEADS_FILE" 2>/dev/null
}

# Whether an explicit `event_type=` literal appears anywhere in the
# description. Accepts `event_type=foo.bar.baz` or `event_type="foo"`.
# Required so the audit-row contract carries a concrete emission name.
bead_audit_block_has_event_type() {
    local bead_id="$1"
    bead_description_text "$bead_id" |
        grep -qE 'event_type[[:space:]]*='
}

# Whether the description names the chain-continuity acceptance criterion.
# We accept either a literal `chain_continuity:` phrase, a `chain_hash`
# token (the column name on the audit rows), or "chain-hash continuity"
# prose (which appears in the bd-3usjw.61 source bead itself).
bead_audit_block_has_chain_continuity() {
    local bead_id="$1"
    bead_description_text "$bead_id" |
        grep -qEi 'chain_continuity|chain_hash|chain-hash continuity'
}

# Whether the description references at least one Rust test file whose
# basename matches `*_audit.rs` AND that file exists on disk. The audit-
# row contract demands an actual test that calls the surface and asserts
# the row landed + verified; the referenced file is the load-bearing
# evidence pointer.
#
# Returns the path on stdout when found (empty otherwise).
bead_audit_block_audit_test_file_path() {
    local bead_id="$1"
    local reference
    for reference in $(bead_referenced_test_paths "$bead_id"); do
        case "$reference" in
            *_audit.rs|*_audit_*.rs)
                if [ -f "$reference" ]; then
                    printf '%s\n' "$reference"
                    return 0
                fi
                ;;
            *_audit*/*.rs)
                # Directory-rooted audit fixture path (rare); allow file
                # existence to confirm.
                if [ -f "$reference" ]; then
                    printf '%s\n' "$reference"
                    return 0
                fi
                ;;
        esac
    done
    return 1
}

# Rule 8: validate AUDIT EMISSION block shape when declared.
#
# Fires per-component (event_type / chain_continuity / audit test file)
# so retrofitted beads with a partial block get specific guidance rather
# than a single opaque "block is incomplete" error.
check_audit_emission_block_shape() {
    local bead_id="$1"
    local surface="$2"

    bead_declares_audit_emission_block "$bead_id" || return 0

    if ! bead_audit_block_has_event_type "$bead_id"; then
        add_violation "$bead_id" "implements-surface" "$surface" \
            "AUDIT EMISSION block declared but missing event_type= literal (missing: event_type)"
    fi

    if ! bead_audit_block_has_chain_continuity "$bead_id"; then
        add_violation "$bead_id" "implements-surface" "$surface" \
            "AUDIT EMISSION block declared but missing chain_continuity acceptance criterion (missing: chain_continuity)"
    fi

    if ! bead_audit_block_audit_test_file_path "$bead_id" >/dev/null; then
        add_violation "$bead_id" "implements-surface" "$surface" \
            "AUDIT EMISSION block declared but no *_audit.rs test file is referenced and on disk (missing: audit_test_file)"
    fi
}

check_referenced_rust_test_file() {
    local bead_id="$1"
    local surface="$2"
    local test_file="$3"

    grep -q '#\[test\]' "$test_file" || return 0
    if rust_test_file_has_only_ignored_tests "$test_file"; then
        add_violation "$bead_id" "implements-surface" "$surface" "$test_file has no non-ignored test"
    elif ! rust_test_file_has_assertion "$test_file"; then
        add_violation "$bead_id" "implements-surface" "$surface" "$test_file lacks assertion-style coverage"
    fi
}

check_referenced_test_paths() {
    local bead_id="$1"
    local surface="$2"
    local reference
    local matches
    local match
    local checked_any

    for reference in $(bead_referenced_test_paths "$bead_id"); do
        matches=$(test_reference_matches "$reference" || true)
        if [ -z "$matches" ]; then
            add_violation "$bead_id" "implements-surface" "$surface" "referenced test path missing: $reference"
            continue
        fi

        checked_any=false
        for match in $matches; do
            case "$match" in
                *.rs)
                    checked_any=true
                    check_referenced_rust_test_file "$bead_id" "$surface" "$match"
                    ;;
            esac
        done

        if [ "$checked_any" = false ] && [ -f "$reference" ]; then
            case "$reference" in
                *.rs)
                    check_referenced_rust_test_file "$bead_id" "$surface" "$reference"
                    ;;
            esac
        fi
    done
}

rust_test_surface_requires_tracing() {
    case "$1" in
        *e2e*.rs|*E2E*.rs) return 0 ;;
        *) return 1 ;;
    esac
}

check_test_tracing_surface_contract() {
    local bead_id="$1"
    local surface="$2"

    [ "$surface" = "e2e_test_logging_convention" ] || return 0

    if [ ! -f "$TEST_TRACING_HELPER" ]; then
        add_violation "$bead_id" "implements-surface" "$surface" "missing $TEST_TRACING_HELPER"
    fi
    if [ ! -d "$TEST_TRACING_LOG_DIR" ] ||
        ! find "$TEST_TRACING_LOG_DIR" -type f 2>/dev/null | grep -q .; then
        add_violation "$bead_id" "implements-surface" "$surface" "missing golden trace log fixtures under $TEST_TRACING_LOG_DIR"
    fi
    if [ -f "tests/test_tracing_support_smoke.rs" ] &&
        ! grep -q 'init_test_tracing' "tests/test_tracing_support_smoke.rs"; then
        add_violation "$bead_id" "implements-surface" "$surface" "test tracing smoke exemplar does not call init_test_tracing"
    fi
}

check_bd3usjw_e2e_test_tracing() {
    local bead_id="$1"
    local surface="$2"
    local test_file

    bead_is_bd3usjw_family "$bead_id" || return 0

    for test_file in $(bead_declared_rust_file_surfaces "$bead_id"); do
        rust_test_surface_requires_tracing "$test_file" || continue
        if [ ! -f "$test_file" ]; then
            add_violation "$bead_id" "implements-surface" "$surface" "declared e2e FILE SURFACE missing: $test_file"
            continue
        fi
        if grep -q '#\[test\]' "$test_file" &&
            ! grep -q 'init_test_tracing' "$test_file"; then
            add_violation "$bead_id" "implements-surface" "$surface" "$test_file does not call init_test_tracing"
        fi
    done
}

bead_degradation_requirement_codes() {
    local bead_id="$1"

    jq -r --arg bead_id "$bead_id" '
        select(.id == $bead_id)
        | [(.description // ""), (.notes // ""), (.close_reason // "")]
        | join("\n")
    ' "$BEADS_FILE" 2>/dev/null |
        awk '
            /DEGRADATION REQUIREMENT/ {
                in_section = 1
                print
                next
            }
            in_section && /^[[:space:]]*(BACKGROUND|WHAT|ACCEPTANCE|FILE SURFACE|TRACING|TEST REQUIREMENT|COST OF OMISSION|PARENT EPIC|DEPENDENCIES)[[:space:]:]/ {
                in_section = 0
            }
            in_section {
                print
            }
        ' |
        grep -Eo 'code[[:space:]]*=[[:space:]]*`?[A-Za-z0-9_.-]+' |
        sed -E 's/^code[[:space:]]*=[[:space:]]*`?//' |
        sort -u || true
}

bead_declares_file_surface_path() {
    local bead_id="$1"
    local expected_path="$2"

    bead_declared_file_surfaces "$bead_id" |
        grep -Fx "$expected_path" >/dev/null 2>&1
}

check_failure_mode_fixture_obligation() {
    local bead_id="$1"
    local surface="$2"
    local code
    local fixture_path

    bead_is_bd3usjw_family "$bead_id" || return 0

    for code in $(bead_degradation_requirement_codes "$bead_id"); do
        fixture_path="$FAILURE_MODE_FIXTURE_DIR/$code.json"
        if [ ! -f "$fixture_path" ]; then
            add_failure_mode_fixture_violation \
                "$bead_id" \
                "$surface" \
                "$fixture_path" \
                "$code" \
                "high" \
                "emitted degraded code missing fixture: $fixture_path"
            continue
        fi

        if ! bead_declares_file_surface_path "$bead_id" "$fixture_path"; then
            add_failure_mode_fixture_violation \
                "$bead_id" \
                "$surface" \
                "$fixture_path" \
                "$code" \
                "medium" \
                "emitted degraded code fixture missing from FILE SURFACE: $fixture_path"
        fi
    done
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

bead_has_rejection_criteria() {
    local bead_id="$1"
    jq -r --arg bead_id "$bead_id" '
        select(.id == $bead_id)
        | [(.description // ""), (.notes // ""), (.close_reason // "")] | join("\n")
    ' "$BEADS_FILE" 2>/dev/null |
        grep -q 'REJECTION CRITERIA'
}

close_reason_has_rejection_threshold_evidence() {
    local close_reason="$1"

    printf "%s\n" "$close_reason" | grep -q 'rejection_threshold_passed:' || return 1
    printf "%s\n" "$close_reason" | grep -q 'measured_value:' || return 1
    printf "%s\n" "$close_reason" | grep -q 'expected_value:' || return 1
    printf "%s\n" "$close_reason" | grep -q 'decision:' || return 1
}

check_math_ambition_closure() {
    local bead_id="$1"
    local close_reason="$2"

    if ! bead_has_rejection_criteria "$bead_id"; then
        add_violation "$bead_id" "math-ambition" "rejection_criteria" "closed math-ambition bead lacks REJECTION CRITERIA"
    fi

    if [ ! -f "$MATH_AMBITION_REJECTION_LOG" ]; then
        add_violation "$bead_id" "math-ambition" "rejection_criteria" "missing $MATH_AMBITION_REJECTION_LOG"
    fi

    if ! close_reason_has_rejection_threshold_evidence "$close_reason"; then
        add_violation "$bead_id" "math-ambition" "rejection_criteria" "close_reason must include rejection_threshold_passed, measured_value, expected_value, and decision fields"
    fi
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
            or ((.labels // []) | index("math-ambition"))
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
                or ((.labels // []) | index("math-ambition"))
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

write_closure_quality_report

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

    if echo "$labels" | grep -qE '\bmath-ambition\b'; then
        check_math_ambition_closure "$bead_id" "$close_reason"
    fi

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

            # Rule 4: New bd-3usjw e2e implementation tests must emit the
            # structured test tracing contract before their bead can close.
            check_test_tracing_surface_contract "$bead_id" "$surface"
            check_bd3usjw_e2e_test_tracing "$bead_id" "$surface"

            # Rule 5: Closed implementation beads that cite test paths must
            # point at real tests, and Rust tests must contain assertions.
            check_referenced_test_paths "$bead_id" "$surface"

            # Rule 6: Closed Part II implementation beads that declare
            # degraded codes must ship and cite their J6 failure-mode fixture.
            check_failure_mode_fixture_obligation "$bead_id" "$surface"

            # Rule 7: Closed implements-surface beads whose FILE SURFACE
            # lists src/ implementation files must include inline
            # #[cfg(test)] unit-test coverage per AGENTS.md L300-302
            # (bd-3usjw.62).
            check_implementation_unit_test_obligation "$bead_id" "$surface"

            # Rule 8: Closed implements-surface beads that declare an
            # AUDIT EMISSION: block must spell out the audit-row contract
            # — event_type, chain-hash continuity criterion, and a real
            # *_audit.rs test file. Validates shape WHEN PRESENT only;
            # the durable_write enforcement gate is a follow-up child.
            # (bd-3usjw.61)
            check_audit_emission_block_shape "$bead_id" "$surface"
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
