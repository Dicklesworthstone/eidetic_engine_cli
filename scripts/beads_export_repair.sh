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

FIXTURE_DIR=""
case "${1:-}" in
    --classify|"") MODE="classify" ;;
    --dry-run) MODE="dry_run" ;;
    --apply) MODE="apply" ;;
    --self-test) MODE="self_test" ;;
    --fixture-suite) MODE="fixture_suite"; FIXTURE_DIR="${2:-tests/fixtures/beads_export}" ;;
    *) echo "usage: $0 [--classify|--dry-run|--apply|--self-test|--fixture-suite [dir]]" >&2; exit 2 ;;
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
    LAST_VALID_LINE_NUMBER=0
    INVALID_LINE_NUMBERS=()
    LAST_INVALID_CLASS="none"
    MERGE_MARKERS=0
    JSONL_AGE_SECONDS=-1

    if [[ -f "$jsonl" ]]; then
        JSONL_EXISTS=true
        local now mtime
        now=$(date +%s)
        # GNU FIRST, AND THE ORDER IS NOT ARBITRARY. BSD `stat -f %m` and GNU
        # `stat -c %Y` are not symmetric fallbacks. On BSD, `-c` is simply
        # invalid: it fails cleanly with no stdout, so the fallback runs. On GNU,
        # `-f` is VALID BUT DIFFERENT -- it means --file-system and treats `%m`
        # as a FILE operand, so it prints filesystem text beginning "  File: ..."
        # AND exits non-zero, which makes the `||` fallback run TOO and append
        # the epoch to that text. BSD-first therefore silently yields a
        # multi-line non-numeric mtime on Linux.
        #
        # That then dies one line down rather than here: inside $(( )) a bare
        # word is a VARIABLE reference, so "File" is looked up and `set -u`
        # aborts with "line 63: File: unbound variable" -- an error naming
        # neither stat nor this script's real problem. Both Beads Export gates
        # failed that way on every Linux run; measured on an RCH worker, while
        # passing 22/22 on macOS.
        mtime=$(stat -c %Y "$jsonl" 2>/dev/null || stat -f %m "$jsonl" 2>/dev/null || echo "$now")
        # Belt and braces: never let a non-numeric mtime reach arithmetic, so a
        # third stat dialect degrades to age 0 instead of killing the gate.
        case "$mtime" in ''|*[!0-9]*) mtime="$now" ;; esac
        JSONL_AGE_SECONDS=$((now - mtime))
        MERGE_MARKERS=$(grep -cE '^(<{7}|={7}|>{7})' "$jsonl" 2>/dev/null)
        MERGE_MARKERS=${MERGE_MARKERS:-0}
        local line_number=0
        while IFS= read -r line || [[ -n "$line" ]]; do
            line_number=$((line_number + 1))
            [[ -z "$line" ]] && continue
            if printf '%s' "$line" | jq -e . >/dev/null 2>&1; then
                JSONL_VALID_RECORDS=$((JSONL_VALID_RECORDS + 1))
                LAST_VALID_LINE_NUMBER=$line_number
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
# States: healthy | invalid_trailing_line | invalid_interior_lines |
#         transient_partial_write | count_divergence_db_behind |
#         count_divergence_jsonl_behind | merge_markers | db_unhealthy | unknown
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
    # Interior corruption (an unparseable line with valid records AFTER it)
    # is not an interrupted-export tail: something wrote into the middle of
    # the file. Fail closed — hand inspection over automated repair.
    local max_invalid=0
    local n
    for n in "${INVALID_LINE_NUMBERS[@]:-}"; do
        [[ -n "$n" && "$n" -gt "$max_invalid" ]] && max_invalid=$n
    done
    if [[ "$invalid_count" -gt 0 && "$LAST_VALID_LINE_NUMBER" -gt "$max_invalid" ]]; then
        STATE="invalid_interior_lines"
        REASON="unparseable line(s) followed by valid records (first invalid at line ${INVALID_LINE_NUMBERS[0]:-?} of ${JSONL_TOTAL_LINES}); this is interior corruption, not an interrupted export tail — inspect by hand before any repair"
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

    # Fixture 7: invalid line in the MIDDLE with valid records after -> fail closed.
    printf '{"id":"a"}\n{"id":"b"\n{"id":"c"}\n' >"$tmp/interior.jsonl"
    touch -t 202601010000 "$tmp/interior.jsonl" 2>/dev/null || true
    gather_evidence "$tmp/interior.jsonl"; DB_DOCTOR_OK=true; DB_COUNT=3
    check "interior corruption fails closed" invalid_interior_lines none

    # ---------------------------------------------------------------------
    # bd-2p297.2: the REPAIR paths, not just the classifier.
    #
    # Everything above calls `classify` directly and asserts STATE. That
    # covers bd-2p297.1 and nothing else: --dry-run and --apply were shipped
    # with no automated coverage at all, while this bead's acceptance asks
    # for "shell/static tests cover safe repair and unsafe refusal cases".
    #
    # These arms drive THIS script as a subprocess against fixture BEADS_DIRs
    # so the real shipped code path runs, with `br` stubbed for hermeticity.
    #
    # The stub is a TRIPWIRE: its `sync` arm always fails loudly. Both --apply
    # arms below are refusal cases that must exit before $REPAIR_COMMAND runs,
    # so if a guard ever regresses the stub fires and the test fails instead
    # of silently mutating a tracker export.
    # ---------------------------------------------------------------------
    # Absolute: repair_check cd's into the fixture root, so a relative
    # BASH_SOURCE would not resolve from there.
    self_script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
    stub_dir="$tmp/stub"
    mkdir -p "$stub_dir"
    cat >"$stub_dir/br" <<'STUB'
#!/usr/bin/env bash
# Record that this stub actually RAN. A stub that exists but is not executable
# is indistinguishable, to every assertion below, from one that worked -- except
# the test would then be exercising the real `br`. The caller asserts this log
# is non-empty, so that fallthrough fails instead of passing quietly.
if [[ -n "${STUB_INVOCATION_LOG:-}" ]]; then
    printf '%s\n' "$*" >>"${STUB_INVOCATION_LOG}"
fi
# Hermetic br stub. Reports a healthy DB with a count supplied by the caller.
for arg in "$@"; do
    case "$arg" in
        doctor) printf '{"workspace_health":"healthy"}\n'; exit 0 ;;
        stats)  printf '{"total":%s}\n' "${STUB_DB_COUNT:-0}"; exit 0 ;;
        sync)
            echo "TRIPWIRE: br sync was invoked during a refusal test" >&2
            exit 99
            ;;
    esac
done
exit 0
STUB
    if ! chmod +x "$stub_dir/br"; then
        echo "FAIL - could not make the br stub executable at "$stub_dir/br"; the" >&2
        echo "       self-test would silently exercise the real br instead." >&2
        exit 3
    fi

    repair_check() {
        local label="$1" mode="$2" beads_dir="$3" db_count="$4" jq_filter="$5" expected="$6"
        local out actual stub_log
        stub_log="$tmp/stub-invocations-${label//[^a-zA-Z0-9]/_}.log"
        : >"$stub_log"
        out=$(cd "$tmp" && STUB_DB_COUNT="$db_count" BEADS_DIR="$beads_dir" \
            STUB_INVOCATION_LOG="$stub_log" \
            PATH="$stub_dir:$PATH" bash "$self_script" "$mode" 2>/dev/null)
        # gather_evidence always runs `br doctor` and `br stats`, so an empty log
        # means the stub was never executed and this case measured the real br.
        if [[ ! -s "$stub_log" ]]; then
            echo "FAIL - $label: the br stub never ran; this case exercised the real br"
            failures=$((failures + 1))
            return
        fi
        actual=$(printf '%s' "$out" | jq -r "$jq_filter" 2>/dev/null)
        if [[ "$actual" == "$expected" ]]; then
            echo "ok   - $label"
        else
            echo "FAIL - $label: $jq_filter got '$actual', wanted '$expected'"
            echo "       raw: $out"
            failures=$((failures + 1))
        fi
    }

    # Fixture A: aged invalid trailing line, DB count corroborating -> the
    # dry run must print the EXACT command and must not mark itself refused.
    mkdir -p "$tmp/safe/.beads"
    printf '{"id":"a"}\n{"id":"b"}\n{"id":"c"\n' >"$tmp/safe/.beads/issues.jsonl"
    touch -t 202601010000 "$tmp/safe/.beads/issues.jsonl" 2>/dev/null || true
    repair_check "dry-run emits the plan schema" --dry-run "safe/.beads" 3 \
        '.schema' 'beads.export_repair_plan.v1'
    repair_check "dry-run prints the exact repair command" --dry-run "safe/.beads" 3 \
        '.wouldRun' 'br sync --flush-only --force --json'
    repair_check "dry-run on a safe candidate is not refused" --dry-run "safe/.beads" 3 \
        '.refused // false' 'false'

    # Fixture B: merge markers -> dry run must refuse and name no command.
    mkdir -p "$tmp/unsafe/.beads"
    printf '{"id":"a"}\n<<<<<<< HEAD\n{"id":"b"}\n' >"$tmp/unsafe/.beads/issues.jsonl"
    repair_check "dry-run refuses merge markers" --dry-run "unsafe/.beads" 2 \
        '.refused' 'true'
    repair_check "refused dry-run offers no command" --dry-run "unsafe/.beads" 2 \
        '.wouldRun' 'null'

    # Fixture C: --apply on a NON-candidate must fail closed before mutating.
    repair_check "apply refuses a non-candidate" --apply "unsafe/.beads" 2 \
        '.applied' 'false'
    unsafe_before=$(shasum -a 256 "$tmp/unsafe/.beads/issues.jsonl" | awk '{print $1}')
    (cd "$tmp" && STUB_DB_COUNT=2 BEADS_DIR="unsafe/.beads" PATH="$stub_dir:$PATH" \
        bash "$self_script" --apply >/dev/null 2>&1)
    unsafe_after=$(shasum -a 256 "$tmp/unsafe/.beads/issues.jsonl" | awk '{print $1}')
    if [[ "$unsafe_before" == "$unsafe_after" ]]; then
        echo "ok   - refused apply left the export byte-identical"
    else
        echo "FAIL - refused apply MUTATED the export"
        failures=$((failures + 1))
    fi

    # Fixture D: the acceptance clause with no coverage until now -- a safe
    # candidate plus an in-flight .beads lock must still refuse.
    mkdir -p "$tmp/locked/.beads"
    printf '{"id":"a"}\n{"id":"b"}\n{"id":"c"\n' >"$tmp/locked/.beads/issues.jsonl"
    touch -t 202601010000 "$tmp/locked/.beads/issues.jsonl" 2>/dev/null || true
    touch "$tmp/locked/.beads/beads.lock"
    repair_check "apply fails closed while a .beads lock exists" --apply "locked/.beads" 3 \
        '.applied' 'false'
    locked_out=$(cd "$tmp" && STUB_DB_COUNT=3 BEADS_DIR="locked/.beads" \
        PATH="$stub_dir:$PATH" bash "$self_script" --apply 2>/dev/null)
    if printf '%s' "$locked_out" | jq -e '.why | test("lock")' >/dev/null 2>&1; then
        echo "ok   - lock refusal explains itself"
    else
        echo "FAIL - lock refusal did not cite the lock: $locked_out"
        failures=$((failures + 1))
    fi

    # Fixture E: the SUCCESS path. This is the arm whose absence hid a real
    # defect -- `set -e` after the repair killed the script inside
    # gather_evidence (grep -c exits 1 when it finds no merge markers), so the
    # report was never printed and the evidence ledger this bead is named for
    # was never written. Every prior arm is a refusal, and refusals return
    # before that line, which is exactly why nothing caught it.
    mkdir -p "$tmp/ok/.beads"
    printf '{"id":"a"}\n{"id":"b"}\n{"id":"c"\n' >"$tmp/ok/.beads/issues.jsonl"
    touch -t 202601010000 "$tmp/ok/.beads/issues.jsonl" 2>/dev/null || true
    repair_stub="$tmp/stub_ok"
    mkdir -p "$repair_stub"
    cat >"$repair_stub/br" <<'STUB'
#!/usr/bin/env bash
# Record that this stub actually RAN. A stub that exists but is not executable
# is indistinguishable, to every assertion below, from one that worked -- except
# the test would then be exercising the real `br`. The caller asserts this log
# is non-empty, so that fallthrough fails instead of passing quietly.
if [[ -n "${STUB_INVOCATION_LOG:-}" ]]; then
    printf '%s\n' "$*" >>"${STUB_INVOCATION_LOG}"
fi
# Stub whose `sync` actually repairs the export, so the success path runs.
for arg in "$@"; do
    case "$arg" in
        doctor) printf '{"workspace_health":"healthy"}\n'; exit 0 ;;
        stats)  printf '{"total":%s}\n' "${STUB_DB_COUNT:-0}"; exit 0 ;;
        sync)
            printf '{"id":"a"}\n{"id":"b"}\n{"id":"c"}\n' >"${BEADS_DIR}/issues.jsonl"
            exit 0
            ;;
    esac
done
exit 0
STUB
    if ! chmod +x "$repair_stub/br"; then
        echo "FAIL - could not make the br stub executable at "$repair_stub/br"; the" >&2
        echo "       self-test would silently exercise the real br instead." >&2
        exit 3
    fi

    ok_stub_log="$tmp/stub-invocations-apply.log"
    : >"$ok_stub_log"
    ok_out=$(cd "$tmp" && STUB_DB_COUNT=3 BEADS_DIR="ok/.beads" \
        STUB_INVOCATION_LOG="$ok_stub_log" \
        PATH="$repair_stub:$PATH" bash "$self_script" --apply 2>/dev/null)
    if [[ -s "$ok_stub_log" ]]; then
        echo "ok   - successful apply ran against the br stub"
    else
        echo "FAIL - successful apply never invoked the br stub; it used the real br"
        failures=$((failures + 1))
    fi
    if [[ "$(printf '%s' "$ok_out" | jq -r '.schema' 2>/dev/null)" == "beads.export_repair_report.v1" ]]; then
        echo "ok   - successful apply emits its report"
    else
        echo "FAIL - successful apply emitted no report: '$ok_out'"
        failures=$((failures + 1))
    fi
    if [[ "$(printf '%s' "$ok_out" | jq -r '.applied' 2>/dev/null)" == "true" ]]; then
        echo "ok   - successful apply reports applied=true"
    else
        echo "FAIL - successful apply did not report applied=true: '$ok_out'"
        failures=$((failures + 1))
    fi
    if [[ -s "$tmp/ok/.beads/export-repair-evidence.jsonl" ]] \
        && jq -e '.preExportSha256 and .postExportSha256 and (.pre.state and .post.state)' \
            "$tmp/ok/.beads/export-repair-evidence.jsonl" >/dev/null 2>&1; then
        echo "ok   - evidence ledger records pre/post hashes and both classifications"
    else
        echo "FAIL - evidence ledger missing or incomplete after a successful apply"
        failures=$((failures + 1))
    fi

    echo "self-test: $((22 - failures))/22 passed"
    [[ "$failures" -eq 0 ]] || exit 2
    exit 0
fi

# --- fixture suite (bd-2p297.3): exercise the classifier against the
# --- committed regression fixtures; never touches the live tracker. ------
if [[ "$MODE" == "fixture_suite" ]]; then
    if [[ ! -d "$FIXTURE_DIR" ]]; then
        echo "fixture dir missing: $FIXTURE_DIR" >&2
        exit 2
    fi
    tmp=$(mktemp -d "${TMPDIR:-/private/tmp}/beads-export-fixtures.XXXXXX")
    failures=0
    ran=0
    for expect in "$FIXTURE_DIR"/*.expect.json; do
        [[ -f "$expect" ]] || continue
        fixture_id=$(basename "$expect" .expect.json)
        jsonl_fixture="$FIXTURE_DIR/${fixture_id}.jsonl"
        if [[ ! -f "$jsonl_fixture" ]]; then
            echo "FAIL - $fixture_id: expect file without jsonl fixture" >&2
            failures=$((failures + 1))
            continue
        fi
        started_ms=$(python3 -c 'import time; print(int(time.time()*1000))' 2>/dev/null || echo 0)
        expected_state=$(jq -r '.expectedState' "$expect")
        expected_candidate=$(jq -r '.expectedCandidate' "$expect")
        # Copy + age the fixture so mtime-based transient detection never
        # misfires on a fresh checkout.
        cp "$jsonl_fixture" "$tmp/${fixture_id}.jsonl"
        touch -t 202601010000 "$tmp/${fixture_id}.jsonl" 2>/dev/null || true
        gather_evidence "$tmp/${fixture_id}.jsonl"
        DB_DOCTOR_OK=$(jq -r '.doctorOk' "$expect")
        [[ "$DB_DOCTOR_OK" == "true" ]] || DB_DOCTOR_OK=false
        DB_COUNT=$(jq -r '.dbCount' "$expect")
        classify
        ended_ms=$(python3 -c 'import time; print(int(time.time()*1000))' 2>/dev/null || echo 0)
        status="pass"
        [[ "$STATE" == "$expected_state" && "$SAFE_REPAIR_CANDIDATE" == "$expected_candidate" ]] || status="fail"
        [[ "$status" == "fail" ]] && failures=$((failures + 1))
        ran=$((ran + 1))
        jq -cn \
            --arg fixture_id "$fixture_id" \
            --arg status "$status" \
            --arg expected_state "$expected_state" \
            --arg actual_state "$STATE" \
            --arg expected_candidate "$expected_candidate" \
            --arg actual_candidate "$SAFE_REPAIR_CANDIDATE" \
            --argjson invalid_lines "${#INVALID_LINE_NUMBERS[@]}" \
            --argjson jsonl_valid "$JSONL_VALID_RECORDS" \
            --argjson db_count "$DB_COUNT" \
            --argjson elapsed_ms "$((ended_ms - started_ms))" \
            '{schema:"ee.test_event.v1",test_id:"beads_export_repair_fixture_suite",kind:"assert_result",
              fields:{label:$fixture_id,status:$status,
                      expectedState:$expected_state,actualState:$actual_state,
                      expectedCandidate:$expected_candidate,actualCandidate:$actual_candidate,
                      invalidLineCount:$invalid_lines,jsonlValidRecords:$jsonl_valid,
                      dbIssueCount:$db_count,elapsedMs:$elapsed_ms,
                      first_failure_diagnosis:(if $status == "fail" then ("expected " + $expected_state + "/" + $expected_candidate + ", got " + $actual_state + "/" + $actual_candidate) else "" end)}}'
    done
    echo "fixture suite: $((ran - failures))/${ran} passed" >&2
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
# Do NOT re-enable errexit here. This script runs under `set -uo pipefail`
# (line 23) and never had `-e`, so the previous `set -e` did not restore a
# prior state -- it INTRODUCED errexit for the rest of the run. The very next
# call, gather_evidence, runs `grep -cE` for merge markers, and grep exits 1
# when it finds none. On a cleanly repaired export that is the normal case, so
# the script died silently right here: no beads.export_repair_report.v1 on
# stdout and no row appended to the evidence ledger.
#
# That made this bead's own deliverable -- the evidence ledger -- unwritable in
# the success path. Failure semantics are already carried explicitly by
# repair_rc and the final `[[ ... ]] || exit 1`, so errexit was never needed.
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
