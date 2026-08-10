#!/usr/bin/env bash
# bd-2p297.1 + bd-2p297.2 — Beads export-integrity classifier and guarded
# forced-flush repair wrapper.
#
# The tracker's JSONL export (.beads/issues.jsonl) can diverge from the
# SQLite DB: a malformed trailing line after an interrupted export, a
# merge-marker collision, or (the dangerous inverse) a healthy JSONL next
# to an empty/failed DB where a forced export would DESTROY the tracker.
# Agents need a deterministic, read-only classification BEFORE anyone
# repairs, and a repair path that fails closed on every ambiguous state.
#
# Modes (mutually exclusive; --classify is the default):
#   --classify        Read-only evidence + classification JSON (beads.export_integrity.v1)
#   --dry-run         Repair preview: prints the exact command and why, or the refusal
#   --apply           Guarded forced export with pre/post evidence appended to
#                     .beads/export-repair-evidence.jsonl
#   --self-test       Fixture-driven classifier checks (no live tracker touched)
#
# Hard rules: never deletes files, never hand-edits JSONL, never runs git,
# never claims source/test verdicts. Output is support-bundle safe: line
# NUMBERS and shape classes only, no raw issue bodies.

set -uo pipefail

BEADS_DIR="${BEADS_DIR:-.beads}"
JSONL="${BEADS_DIR}/issues.jsonl"
LEDGER="${BEADS_DIR}/export-repair-evidence.jsonl"
SCHEMA="beads.export_integrity.v1"
MODE="classify"

case "${1:-}" in
    --classify|"") MODE="classify" ;;
    --dry-run) MODE="dry_run" ;;
    --apply) MODE="apply" ;;
    --self-test) MODE="self_test" ;;
    *) echo "usage: $0 [--classify|--dry-run|--apply|--self-test]" >&2; exit 2 ;;
esac

if ! command -v jq >/dev/null 2>&1; then
    echo '{"schema":"'"$SCHEMA"'","state":"unknown","mutationSafe":false,"reason":"jq missing"}'
    exit 3
fi

# --- evidence gathering (read-only) --------------------------------------
gather_evidence() {
    local jsonl="$1"
    JSONL_EXISTS=false
    JSONL_TOTAL_LINES=0
    JSONL_VALID_RECORDS=0
    INVALID_LINE_NUMBERS=()
    LAST_INVALID_CLASS="none"
    MERGE_MARKERS=0
    JSONL_AGE_SECONDS=-1

    if [[ -f "$jsonl" ]]; then
        JSONL_EXISTS=true
        local now mtime
        now=$(date +%s)
        mtime=$(stat -f %m "$jsonl" 2>/dev/null || stat -c %Y "$jsonl" 2>/dev/null || echo "$now")
        JSONL_AGE_SECONDS=$((now - mtime))
        MERGE_MARKERS=$(grep -cE '^(<{7}|={7}|>{7})' "$jsonl" 2>/dev/null)
        MERGE_MARKERS=${MERGE_MARKERS:-0}
        local line_number=0
        while IFS= read -r line || [[ -n "$line" ]]; do
            line_number=$((line_number + 1))
            [[ -z "$line" ]] && continue
            if printf '%s' "$line" | jq -e . >/dev/null 2>&1; then
                JSONL_VALID_RECORDS=$((JSONL_VALID_RECORDS + 1))
            else
                if [[ ${#INVALID_LINE_NUMBERS[@]} -lt 5 ]]; then
                    INVALID_LINE_NUMBERS+=("$line_number")
                fi
                # Shape class only — never the content.
                case "$line" in
                    '{'*) LAST_INVALID_CLASS="truncated_json_object" ;;
                    '<<<<<<<'*|'======='*|'>>>>>>>'*) LAST_INVALID_CLASS="merge_marker" ;;
                    *) LAST_INVALID_CLASS="non_json_garbage" ;;
                esac
            fi
        done <"$jsonl"
        JSONL_TOTAL_LINES=$line_number
    fi

    DB_DOCTOR_OK=false
    DB_COUNT=-1
    # br doctor exits non-zero for ANY failing check (incl. advisory dep
    # hygiene), so health comes from the JSON body, not the exit code.
    DOCTOR_JSON=$(br doctor --json --no-db 2>/dev/null || true)
    if [[ -n "$DOCTOR_JSON" ]] \
        && [[ "$(printf '%s' "$DOCTOR_JSON" | jq -r '.workspace_health // empty' 2>/dev/null)" == "healthy" ]]; then
        DB_DOCTOR_OK=true
    fi
    if STATS_JSON=$(br --no-auto-import stats --json 2>/dev/null) && [[ -n "$STATS_JSON" ]]; then
        DB_COUNT=$(printf '%s' "$STATS_JSON" | jq -r '.total // .total_issues // .totalIssues // -1' 2>/dev/null || echo -1)
    fi
}

# --- classification ------------------------------------------------------
# States: healthy | invalid_trailing_line | transient_partial_write |
#         count_divergence_db_behind | count_divergence_jsonl_behind |
#         merge_markers | db_unhealthy | unknown
classify() {
    local invalid_count=${#INVALID_LINE_NUMBERS[@]}
    STATE="unknown"
    MUTATION_SAFE=false
    SAFE_REPAIR_CANDIDATE="none"
    REASON=""

    if [[ "$JSONL_EXISTS" != true ]]; then
        STATE="unknown"
        REASON="no JSONL export present; nothing classifiable"
        return
    fi
    if [[ "$MERGE_MARKERS" -gt 0 ]]; then
        STATE="merge_markers"
        REASON="JSONL carries ${MERGE_MARKERS} merge-marker line(s); resolve the collision by hand — forced export would silently pick one side"
        return
    fi
    if [[ "$DB_DOCTOR_OK" != true ]]; then
        STATE="db_unhealthy"
        REASON="br doctor did not report a readable healthy DB; no repair direction is safe without DB integrity evidence"
        return
    fi
    if [[ "$invalid_count" -eq 0 ]]; then
        if [[ "$DB_COUNT" -ge 0 && "$JSONL_VALID_RECORDS" -gt 0 && "$DB_COUNT" -lt $((JSONL_VALID_RECORDS - 5)) ]]; then
            STATE="count_divergence_db_behind"
            REASON="DB reports ${DB_COUNT} issues but JSONL holds ${JSONL_VALID_RECORDS} valid records; a forced DB export would DESTROY tracker data — repair direction is import, not export"
            return
        fi
        STATE="healthy"
        MUTATION_SAFE=true
        REASON="all ${JSONL_VALID_RECORDS} JSONL records parse and DB evidence is consistent"
        return
    fi
    # Invalid lines present.
    if [[ "$JSONL_AGE_SECONDS" -ge 0 && "$JSONL_AGE_SECONDS" -lt 10 ]]; then
        STATE="transient_partial_write"
        REASON="JSONL modified ${JSONL_AGE_SECONDS}s ago with ${invalid_count} unparseable line(s); an export may be mid-flight — re-classify after a short wait instead of repairing"
        return
    fi
    if [[ "$DB_COUNT" -ge 0 && "$JSONL_VALID_RECORDS" -gt 0 ]]; then
        local delta=$((DB_COUNT - JSONL_VALID_RECORDS))
        [[ $delta -lt 0 ]] && delta=$((-delta))
        if [[ $delta -le 5 && "$LAST_INVALID_CLASS" != "merge_marker" ]]; then
            STATE="invalid_trailing_line"
            SAFE_REPAIR_CANDIDATE="forced_export"
            REASON="DB healthy with ${DB_COUNT} issues vs ${JSONL_VALID_RECORDS} valid JSONL records (delta ${delta}); ${invalid_count} unparseable line(s) look like an interrupted export tail — 'br sync --flush-only --force --json' is a candidate ONLY because DB integrity and counts corroborate"
            return
        fi
        if [[ "$DB_COUNT" -lt "$JSONL_VALID_RECORDS" ]]; then
            STATE="count_divergence_db_behind"
            REASON="DB (${DB_COUNT}) is behind valid JSONL records (${JSONL_VALID_RECORDS}); forced export would destroy data"
        else
            STATE="count_divergence_jsonl_behind"
            REASON="valid JSONL records (${JSONL_VALID_RECORDS}) trail the DB (${DB_COUNT}) by more than the tail-repair tolerance; evidence is ambiguous"
        fi
        return
    fi
    STATE="unknown"
    REASON="DB count unavailable while JSONL has ${invalid_count} unparseable line(s); evidence is ambiguous — stop Beads mutation and escalate"
}

emit_classification() {
    jq -cn \
        --arg schema "$SCHEMA" \
        --arg state "$STATE" \
        --arg reason "$REASON" \
        --arg candidate "$SAFE_REPAIR_CANDIDATE" \
        --arg invalid_class "$LAST_INVALID_CLASS" \
        --argjson mutation_safe "$MUTATION_SAFE" \
        --argjson jsonl_total "$JSONL_TOTAL_LINES" \
        --argjson jsonl_valid "$JSONL_VALID_RECORDS" \
        --argjson db_count "$DB_COUNT" \
        --argjson merge_markers "$MERGE_MARKERS" \
        --argjson age_seconds "$JSONL_AGE_SECONDS" \
        --argjson invalid_lines "$(printf '%s\n' "${INVALID_LINE_NUMBERS[@]:-}" | jq -R 'select(length>0) | tonumber' | jq -sc .)" \
        '{schema:$schema,state:$state,mutationSafe:$mutation_safe,safeRepairCandidate:$candidate,reason:$reason,
          evidence:{jsonlTotalLines:$jsonl_total,jsonlValidRecords:$jsonl_valid,dbIssueCount:$db_count,
                    invalidLineNumbers:$invalid_lines,lastInvalidLineClass:$invalid_class,
                    mergeMarkerLines:$merge_markers,jsonlAgeSeconds:$age_seconds}}'
}

# --- self-test (fixtures only; no live tracker reads) --------------------
if [[ "$MODE" == "self_test" ]]; then
    tmp=$(mktemp -d "${TMPDIR:-/private/tmp}/beads-export-repair-test.XXXXXX")
    failures=0
    check() {
        local label="$1" expected_state="$2" expected_candidate="$3"
        classify
        if [[ "$STATE" == "$expected_state" && "$SAFE_REPAIR_CANDIDATE" == "$expected_candidate" ]]; then
            echo "ok   - $label ($STATE)"
        else
            echo "FAIL - $label: got state=$STATE candidate=$SAFE_REPAIR_CANDIDATE, wanted $expected_state/$expected_candidate"
            failures=$((failures + 1))
        fi
    }

    # Fixture 1: clean tail, counts match -> healthy.
    printf '{"id":"a"}\n{"id":"b"}\n' >"$tmp/clean.jsonl"
    gather_evidence "$tmp/clean.jsonl"; DB_DOCTOR_OK=true; DB_COUNT=2
    check "clean export" healthy none

    # Fixture 2: truncated trailing object, counts corroborate -> repair candidate.
    printf '{"id":"a"}\n{"id":"b"}\n{"id":"c"\n' >"$tmp/tail.jsonl"
    touch -t 202601010000 "$tmp/tail.jsonl" 2>/dev/null || true
    gather_evidence "$tmp/tail.jsonl"; DB_DOCTOR_OK=true; DB_COUNT=3
    check "invalid trailing line" invalid_trailing_line forced_export

    # Fixture 3: SAME shape but DB empty -> forced export would destroy data.
    gather_evidence "$tmp/clean.jsonl"; DB_DOCTOR_OK=true; DB_COUNT=0
    JSONL_VALID_RECORDS=4244 # today's live incident shape
    check "db behind jsonl (destructive-export refusal)" count_divergence_db_behind none

    # Fixture 4: merge markers -> stop.
    printf '{"id":"a"}\n<<<<<<< HEAD\n{"id":"b"}\n' >"$tmp/merge.jsonl"
    gather_evidence "$tmp/merge.jsonl"; DB_DOCTOR_OK=true; DB_COUNT=2
    check "merge markers" merge_markers none

    # Fixture 5: unhealthy DB -> stop regardless of tail shape.
    gather_evidence "$tmp/tail.jsonl"; DB_DOCTOR_OK=false; DB_COUNT=-1
    check "db unhealthy" db_unhealthy none

    # Fixture 6: fresh mtime -> transient partial write.
    printf '{"id":"a"}\n{"id":"b"\n' >"$tmp/fresh.jsonl"
    gather_evidence "$tmp/fresh.jsonl"; DB_DOCTOR_OK=true; DB_COUNT=2
    check "transient partial write" transient_partial_write none

    echo "self-test: $((6 - failures))/6 passed"
    [[ "$failures" -eq 0 ]] || exit 2
    exit 0
fi

# --- live modes ----------------------------------------------------------
gather_evidence "$JSONL"
classify
CLASSIFICATION=$(emit_classification)

if [[ "$MODE" == "classify" ]]; then
    printf '%s\n' "$CLASSIFICATION"
    exit 0
fi

REPAIR_COMMAND="br sync --flush-only --force --json"

if [[ "$MODE" == "dry_run" ]]; then
    if [[ "$SAFE_REPAIR_CANDIDATE" == "forced_export" ]]; then
        jq -cn --argjson classification "$CLASSIFICATION" --arg command "$REPAIR_COMMAND" \
            '{schema:"beads.export_repair_plan.v1",wouldRun:$command,apply:false,
              why:$classification.reason,classification:$classification}'
    else
        jq -cn --argjson classification "$CLASSIFICATION" \
            '{schema:"beads.export_repair_plan.v1",wouldRun:null,apply:false,
              refused:true,why:$classification.reason,classification:$classification}'
    fi
    exit 0
fi

# --apply
if [[ "$SAFE_REPAIR_CANDIDATE" != "forced_export" ]]; then
    jq -cn --argjson classification "$CLASSIFICATION" \
        '{schema:"beads.export_repair_report.v1",applied:false,refused:true,
          why:$classification.reason,classification:$classification}'
    exit 1
fi
# Fail closed if another Beads mutation looks in flight.
if compgen -G "${BEADS_DIR}/*.lock" >/dev/null 2>&1; then
    jq -cn --argjson classification "$CLASSIFICATION" \
        '{schema:"beads.export_repair_report.v1",applied:false,refused:true,
          why:"a .beads lock file exists; another Beads mutation may be in flight",classification:$classification}'
    exit 1
fi

pre_hash=$(shasum -a 256 "$JSONL" 2>/dev/null | awk '{print $1}')
set +e
REPAIR_OUTPUT=$($REPAIR_COMMAND 2>&1)
repair_rc=$?
set -e 2>/dev/null || true
post_hash=$(shasum -a 256 "$JSONL" 2>/dev/null | awk '{print $1}')
gather_evidence "$JSONL"
classify
POST_CLASSIFICATION=$(emit_classification)
post_doctor_ok=false
br doctor --json --no-db >/dev/null 2>&1 && post_doctor_ok=true

REPORT=$(jq -cn \
    --argjson pre "$CLASSIFICATION" \
    --argjson post "$POST_CLASSIFICATION" \
    --arg command "$REPAIR_COMMAND" \
    --arg pre_hash "${pre_hash:-unknown}" \
    --arg post_hash "${post_hash:-unknown}" \
    --argjson rc "$repair_rc" \
    --argjson doctor_ok "$post_doctor_ok" \
    --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{schema:"beads.export_repair_report.v1",applied:($rc == 0),command:$command,exitCode:$rc,
      recordedAt:$ts,preExportSha256:$pre_hash,postExportSha256:$post_hash,
      postDoctorOk:$doctor_ok,pre:$pre,post:$post}')
printf '%s\n' "$REPORT"
printf '%s\n' "$REPORT" >>"$LEDGER"
[[ "$repair_rc" -eq 0 && "$post_doctor_ok" == true ]] || exit 1
exit 0
