#!/usr/bin/env bash
# bd-2vq2z.20 - Capture-track real-binary E2E route.
#
# Scenario: run a prebuilt ee binary against an isolated workspace and fixture
# git/CASS inputs. Pin ambient capture suggestions, git-derived remember capture,
# and session-arc review as proposal/accept-only flows with structured
# ee.test_event.v1 logging. The capture features are closed; route gaps are
# assertion failures, not capability skips.
#
# NOTE: no `set -e`; harness assertions accumulate and harness_summary owns the
# exit code so failures still write artifacts.
set -uo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Avoid the shared harness's cargo-metadata fallback in code-first swarm lanes.
EE_BIN="${EE_BIN:-ee}"
export EE_BIN

# Capture proofs are forensic artifacts for review/convergence. Retain the
# isolated workspace by default; callers can opt out explicitly.
export EE_E2E_KEEP="${EE_E2E_KEEP:-1}"

# shellcheck source=scripts/e2e_lib.sh
# shellcheck disable=SC1091
source "$E2E_DIR/e2e_lib.sh"

harness_init "capture"

# Callers capture command output with `$(ee_json ...)`, which runs the helper
# in a subshell. Record nonzero results durably and fold them into the parent
# harness before summary so binary and logger failures cannot be masked.
EE_JSON_FAILURES_FILE="$LOG_DIR/command-failures.log"
: >"$EE_JSON_FAILURES_FILE"

ee_json() {
    local rc=0
    e2e_log_command "$EE_BIN" "$@" || rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'exit=%s command=%s\n' "$rc" "$*" >>"$EE_JSON_FAILURES_FILE"
    fi
    return "$rc"
}

ee_json_with_env() {
    local env_key="$1"
    local env_value="$2"
    shift 2
    local rc=0
    EE_E2E_ARTIFACT_BINARY="$EE_BIN" \
        e2e_log_command env "$env_key=$env_value" "$EE_BIN" "$@" || rc=$?
    if [ "$rc" -ne 0 ]; then
        printf 'exit=%s env=%s command=%s\n' "$rc" "$env_key" "$*" \
            >>"$EE_JSON_FAILURES_FILE"
    fi
    return "$rc"
}

assert_nonempty() {
    local value="$1"
    local label="$2"
    if [ -n "$value" ] && [ "$value" != "null" ]; then
        e2e_log_assert_eq "nonempty" "nonempty" "$label" || true
        _harness_pass "$label"
    else
        e2e_log_assert_eq "empty" "nonempty" "$label" || true
        _harness_fail "$label: value was empty"
    fi
}

assert_distinct() {
    local left="$1"
    local right="$2"
    local label="$3"
    if [ -n "$left" ] && [ -n "$right" ] && [ "$left" != "$right" ]; then
        e2e_log_assert_eq "distinct" "distinct" "$label" || true
        _harness_pass "$label"
    else
        e2e_log_assert_eq "equal-or-empty" "distinct" "$label" || true
        _harness_fail "$label: values must both be nonempty and distinct"
    fi
}

assert_zero() {
    local value="$1"
    local label="$2"
    if [ "${value:-}" = "0" ]; then
        e2e_log_assert_eq "$value" "0" "$label" || true
        _harness_pass "$label"
    else
        e2e_log_assert_eq "${value:-missing}" "0" "$label" || true
        _harness_fail "$label: expected 0 got ${value:-missing}"
    fi
}

capture_suggest_available() {
    "$EE_BIN" capture suggest --help >/dev/null 2>&1
}

review_session_available() {
    "$EE_BIN" review session --help >/dev/null 2>&1
}

remember_git_capture_available() {
    "$EE_BIN" remember --help 2>&1 | grep -q -- "--from-commit" \
        && "$EE_BIN" remember --help 2>&1 | grep -q -- "--from-diff" \
        && "$EE_BIN" remember --help 2>&1 | grep -q -- "--apply"
}

curate_apply_available() {
    "$EE_BIN" curate apply --help >/dev/null 2>&1
}

audit_timeline_available() {
    "$EE_BIN" audit timeline --help >/dev/null 2>&1
}

memory_list_count() {
    local workspace="$1"
    local out
    out="$(ee_json --workspace "$workspace" memory list --json)"
    printf '%s' "$out" | jq -r '
        (.data.memories // .data.items // .data.results // []) | length
    ' 2>/dev/null || printf '%s\n' "0"
}

candidate_count() {
    local workspace="$1"
    local out
    out="$(ee_json --workspace "$workspace" curate candidates --all --json)"
    printf '%s' "$out" | jq -r '
        (.data.candidates // .data.items // .data.results // []) | length
    ' 2>/dev/null || printf '%s\n' "0"
}

candidate_id_at() {
    local json="$1"
    local index="$2"
    printf '%s' "$json" | jq -r --argjson index "$index" '
        (.data.candidates // .data.items // .data.results // [])[$index]
        | (.candidateId // .candidate_id // .id // empty)
    ' 2>/dev/null | head -n 1
}

candidate_type_count() {
    local json="$1"
    local pattern="$2"
    printf '%s' "$json" | jq -r --arg pattern "$pattern" '
        [
            (.data.candidates // .data.items // .data.results // [])[]?
            | select(((.type // .candidateType // .topicKey // .kind // .reason // "") | tostring | test($pattern; "i")))
        ] | length
    ' 2>/dev/null || printf '%s\n' "0"
}

write_fake_cass_binary() {
    local path="$1"
    local session_path="$2"
    local second_session_path="$3"
    cat >"$path" <<FAKECASS
#!/bin/sh
set -eu
cmd="\${1:-}"
case "\$cmd" in
  sessions)
    cat <<'JSON'
{"sessions":[{"path":"$session_path","workspace":"$WS","agent":"codex","started_at":"2026-06-17T13:00:00Z","ended_at":"2026-06-17T13:20:00Z","message_count":4,"token_count":920,"content_hash":"blake3:capture-e2e-session-primary"},{"path":"$second_session_path","workspace":"$WS","agent":"claude","started_at":"2026-06-17T14:00:00Z","ended_at":"2026-06-17T14:10:00Z","message_count":2,"token_count":310,"content_hash":"blake3:capture-e2e-session-secondary"}]}
JSON
    ;;
  view)
    source_path=""
    for arg in "\$@"; do source_path="\$arg"; done
    if [ "\$source_path" = "$session_path" ]; then
      cat <<'JSON'
{"path":"$session_path","target_line":2,"context":3,"lines":[
  {"line":1,"content":"{\"role\":\"user\",\"content\":\"The capture workflow kept reproposing the same lesson after acceptance.\"}","highlighted":false},
  {"line":2,"content":"{\"role\":\"assistant\",\"content\":\"Lesson: ambient capture must dedupe accepted suggestions and route storage through explicit curation accept.\"}","highlighted":true},
  {"line":3,"content":"{\"role\":\"assistant\",\"content\":\"Failure arc: storing silently would violate the no-loop-takeover policy.\"}","highlighted":false},
  {"line":4,"content":"{\"role\":\"user\",\"content\":\"Fix: require accept/reject commands and audit every accepted capture.\"}","highlighted":false}
],"total_lines":4}
JSON
    elif [ "\$source_path" = "$second_session_path" ]; then
      cat <<'JSON'
{"path":"$second_session_path","target_line":1,"context":3,"lines":[
  {"line":1,"content":"{\"role\":\"user\",\"content\":\"A second session makes coalescing observable instead of inferred.\"}","highlighted":true},
  {"line":2,"content":"{\"role\":\"assistant\",\"content\":\"Batch both durable index jobs into one published source snapshot.\"}","highlighted":false}
],"total_lines":2}
JSON
    else
      echo "unexpected cass view path: \$source_path" >&2
      exit 64
    fi
    ;;
  *)
    echo "unexpected cass command: \$cmd" >&2
    exit 64
    ;;
esac
FAKECASS
    chmod 755 "$path"
}

assert_audit_mentions_capture() {
    local workspace="$1"
    local label="$2"
    if audit_timeline_available; then
        audit_out="$(ee_json --workspace "$workspace" audit timeline --limit 50 --json)"
        assert_jq "$audit_out" '.schema == "ee.response.v2" and .success == true' \
            "$label audit timeline succeeds"
        assert_jq "$audit_out" '
            tostring | test("curation|candidate|memory|review|capture|remember|audit"; "i")
        ' "$label audit timeline mentions the accepted capture path"
    else
        e2e_log_assert_eq "missing" "available" "$label audit timeline route available" || true
        _harness_fail "$label: audit timeline route is required to prove accepted capture writes an audit row"
    fi
}

with_temp_workspace WS
FIXTURE_REPO="$WS/capture-fixture-repo"
SESSION_PATH="$WS/cass-session-capture.jsonl"
SECOND_SESSION_PATH="$WS/cass-session-capture-secondary.jsonl"
CASS_BIN="$WS/cass"
SECRET="sk-proj-capture-e2e-redacted-000000000000000000"

step "init capture workspace and fixture inputs"
mkdir -p "$FIXTURE_REPO"
init_out="$(ee_json --workspace "$WS" init --json)"
assert_jq "$init_out" '.schema == "ee.response.v2" and .success == true' \
    "ee init succeeds for capture workspace"

cat >"$SESSION_PATH" <<'SESSION'
{"role":"user","content":"The capture workflow kept reproposing the same lesson after acceptance."}
{"role":"assistant","content":"Lesson: ambient capture must dedupe accepted suggestions and route storage through explicit curation accept."}
{"role":"assistant","content":"Failure arc: storing silently would violate the no-loop-takeover policy."}
{"role":"user","content":"Fix: require accept/reject commands and audit every accepted capture."}
SESSION
cat >"$SECOND_SESSION_PATH" <<'SESSION'
{"role":"user","content":"A second session makes coalescing observable instead of inferred."}
{"role":"assistant","content":"Batch both durable index jobs into one published source snapshot."}
SESSION
write_fake_cass_binary "$CASS_BIN" "$SESSION_PATH" "$SECOND_SESSION_PATH"
log_event "capture_fixture_ready" \
    bead "bd-2vq2z.20" \
    workspace "$WS" \
    fixtureRepo "$FIXTURE_REPO" \
    cassSession "$SESSION_PATH" \
    cassSessionSecondary "$SECOND_SESSION_PATH"

step "import fixture CASS session without silent memory mutation"
before_import_memories="$(memory_list_count "$WS")"
import_out="$(ee_json_with_env EE_CASS_BINARY "$CASS_BIN" --workspace "$WS" import cass --limit 2 --json)"
assert_jq "$import_out" '.schema == "ee.response.v2" and .success == true' \
    "fixture cass import succeeds"
assert_jq "$import_out" '
    (.data.schema == "ee.import.cass.v1")
    and (.data.sessionsImported == 2)
    and (.data.spansImported == 6)
    and (.data.indexJobsQueued == 2)
    and (.data.sessions | length == 2)
    and all(.data.sessions[]; (.sessionId | type == "string" and length > 0)
        and (.indexJobId | type == "string" and length > 0))
' "fixture cass import stores two sessions, six evidence spans, and two durable index jobs"
spans_imported="$(printf '%s' "$import_out" | jq -r '.data.spansImported // 0')"
first_session_id="$(printf '%s' "$import_out" | jq -r '.data.sessions[0].sessionId // empty')"
second_session_id="$(printf '%s' "$import_out" | jq -r '.data.sessions[1].sessionId // empty')"
first_index_job_id="$(printf '%s' "$import_out" | jq -r '.data.sessions[0].indexJobId // empty')"
second_index_job_id="$(printf '%s' "$import_out" | jq -r '.data.sessions[1].indexJobId // empty')"
assert_nonempty "$first_session_id" "first imported session exposes its exact ID"
assert_nonempty "$second_session_id" "second imported session exposes its exact ID"
assert_nonempty "$first_index_job_id" "first import exposes its exact index job ID"
assert_nonempty "$second_index_job_id" "second import exposes its exact index job ID"
assert_distinct "$first_session_id" "$second_session_id" \
    "imported sessions expose distinct IDs"
assert_distinct "$first_index_job_id" "$second_index_job_id" \
    "imported sessions queue distinct index job IDs"
after_import_memories="$(memory_list_count "$WS")"
assert_zero "$after_import_memories" \
    "cass import creates evidence but does not silently store memories"
assert_zero "$before_import_memories" \
    "capture workspace starts without memories"

# bd-3k1mg: exercise the coalesced durable job path. A manual index rebuild here
# would hide a missing or incomplete import job, and an ID-or-content assertion
# could pass when two different documents accidentally satisfy half the proof.
step "bd-3k1mg: two index jobs coalesce into one complete evidence snapshot"
stale_index_out="$(ee_json --workspace "$WS" index status --json)"
assert_jq "$stale_index_out" ".schema == \"ee.response.v2\"
    and .success == true
    and .data.health == \"stale\"
    and .data.dbSessionCount == 2
    and .data.dbEvidenceCount == $spans_imported
    and .data.dbEvidenceAdmittedCount == $spans_imported
    and (.data.dbGeneration > .data.indexGeneration)" \
    "atomic CASS import makes the previously ready index truthfully stale"
coalesce_out="$(ee_json --workspace "$WS" job run index_coalesce --item-limit 2 --json)"
assert_jq "$coalesce_out" ".schema == \"ee.response.v2\" and .success == true
    and .data.requestedJob == \"index_coalesce\"
    and .data.durableMutation == true
    and .data.summary == {\"total\":1,\"succeeded\":1,\"skipped\":0,\"failed\":0}
    and .data.job.outcome == \"success\"
    and .data.job.details.schema == \"ee.steward.index_coalesce.v1\"
    and .data.job.details.preflight.pending_jobs == 2
    and .data.job.details.result.status == \"success\"
    and .data.job.details.result.pending_jobs == 2
    and .data.job.details.result.processed_jobs == 2
    and .data.job.details.result.completed_jobs == 2
    and .data.job.details.result.failed_jobs == 0
    and (.data.job.details.result.jobs | length == 2)
    and ([.data.job.details.result.jobs[].processing_mode] | unique | length == 1)
    and ([.data.job.details.result.jobs[].fallback_to_full] | unique | length == 1)
    and any(.data.job.details.result.jobs[];
        .job_id == \"$first_index_job_id\"
        and .document_id == \"$first_session_id\"
        and .document_source == \"session\"
        and .outcome == \"completed\"
        and ((.processing_mode == \"coalesced_full_rebuild_staged_full_rebuild\"
                and .fallback_to_full == null)
            or (.processing_mode == \"coalesced_full_rebuild_fallback_to_full\"
                and (.fallback_to_full | type) == \"string\"
                and (.fallback_to_full | length) > 0))
        and .documents_total == (2 + $spans_imported)
        and .documents_indexed == (2 + $spans_imported))
    and any(.data.job.details.result.jobs[];
        .job_id == \"$second_index_job_id\"
        and .document_id == \"$second_session_id\"
        and .document_source == \"session\"
        and .outcome == \"completed\"
        and ((.processing_mode == \"coalesced_full_rebuild_staged_full_rebuild\"
                and .fallback_to_full == null)
            or (.processing_mode == \"coalesced_full_rebuild_fallback_to_full\"
                and (.fallback_to_full | type) == \"string\"
                and (.fallback_to_full | length) > 0))
        and .documents_total == (2 + $spans_imported)
        and .documents_indexed == (2 + $spans_imported))" \
    "public index_coalesce binds both import jobs to one completed source snapshot"
ready_index_out="$(ee_json --workspace "$WS" index status --json)"
assert_jq "$ready_index_out" ".schema == \"ee.response.v2\"
    and .success == true
    and .data.health == \"ready\"
    and (.data.dbGeneration == .data.indexGeneration)
    and .data.dbSessionCount == 2
    and .data.dbEvidenceCount == $spans_imported
    and .data.dbEvidenceAdmittedCount == $spans_imported
    and .data.indexDocumentCounts.sessions == 2
    and .data.indexDocumentCounts.evidence == $spans_imported
    and .data.indexDocumentCount == (2 + $spans_imported)" \
    "coalesced job drain publishes exact session/evidence counts at the DB generation"

# bd-16imy: the exact admitted transcript span must flow through search and
# directly into a deterministic pack without a fabricated MemoryId.
step "bd-16imy: imported transcript phrase is searchable and directly packable"
evidence_search_out="$(ee_json --workspace "$WS" search \
    "ambient capture must dedupe accepted suggestions" --limit 20 --json)"
assert_jq "$evidence_search_out" '.schema == "ee.response.v2" and .success == true' \
    "search for imported transcript phrase succeeds"
exact_evidence_uri="cass-session://$first_session_id#L2-2"
exact_evidence_content='{"role":"assistant","content":"Lesson: ambient capture must dedupe accepted suggestions and route storage through explicit curation accept."}'
exact_evidence_content_json="$(printf '%s' "$exact_evidence_content" | jq -Rs '.')"
exact_evidence_matches="$(printf '%s' "$evidence_search_out" | jq -c \
    --arg content "$exact_evidence_content" \
    --arg session "$first_session_id" \
    --arg uri "$exact_evidence_uri" '
    [
        .data.results[]? as $hit
        | select(
            (($hit.docId // "") | startswith("ev_"))
            and $hit.content == $content
            and $hit.metadata.session_id == $session
            and $hit.metadata.start_line == "2"
            and $hit.metadata.end_line == "2"
            and any($hit.provenance[]?;
                .kind == "provenance_uri" and .uri == $uri)
            and any($hit.provenance[]?;
                .kind == "search_document" and .docId == $hit.docId)
        )
        | {docId: $hit.docId}
    ]')"
assert_json "$exact_evidence_matches" 'length' '1' \
    "exactly one imported evidence hit binds ID, full content, session, line range, and provenance"
evidence_doc_id="$(printf '%s' "$exact_evidence_matches" | jq -r '.[0].docId // empty')"
assert_nonempty "$evidence_doc_id" "search exposes the exact imported evidence document ID"
evidence_pack_out="$(ee_json --workspace "$WS" pack \
    "ambient capture must dedupe accepted suggestions" --max-tokens 2000 --json)"
assert_jq "$evidence_pack_out" '.schema == "ee.response.v2" and .success == true' \
    "pack for imported transcript phrase succeeds"
assert_jq "$evidence_pack_out" ".data.pack.schema == \"ee.pack.v2\"
    and (.data.pack.items | type == \"array\")
    and (.data.pack.items | length == 1)
    and .data.pack.items[0].entityKind == \"evidence_span\"
    and .data.pack.items[0].evidenceSpanId == \"$evidence_doc_id\"
    and (.data.pack.items[0] | has(\"memoryId\") | not)
    and .data.pack.items[0].content == $exact_evidence_content_json
    and .data.pack.items[0].sessionId == \"$first_session_id\"
    and .data.pack.items[0].startLine == 2
    and .data.pack.items[0].endLine == 2
    and (.data.pack.items[0].entityRevision | startswith(\"blake3:\"))
    and .data.pack.items[0].trust.class == \"cass_evidence\"
    and (.data.pack.items[0].why | contains(\"$evidence_doc_id\"))
    and any(.data.pack.items[0].provenance[]?; .uri == \"$exact_evidence_uri\")
    and .data.pack.items[0].sourceIndex == 1
    and .data.pack.quality.itemCount == 1
    and .data.pack.selectionAudit.selectedCount == 1
    and .data.pack.provenanceFooter.evidenceCount == 1
    and all((.degraded // [])[]?;
        .code != \"context_evidence_hit_unhydrated\"
        and .code != \"context_pack_persist_failed\")" \
    "fresh imported evidence is the sole typed pack item with exact content, identity, why, and line provenance"
repeat_evidence_pack_out="$(ee_json --workspace "$WS" pack \
    "ambient capture must dedupe accepted suggestions" --max-tokens 2000 --json)"
assert_jq "$repeat_evidence_pack_out" ".success == true
    and .data.pack.hash == $(printf '%s' "$evidence_pack_out" | jq -c '.data.pack.hash')
    and .data.pack.items == $(printf '%s' "$evidence_pack_out" | jq -c '.data.pack.items')
    and .data.pack.budget.usedTokens == $(printf '%s' "$evidence_pack_out" | jq -c '.data.pack.budget.usedTokens')" \
    "repeating the evidence pack preserves hash, typed item bytes, and truthful token accounting"

ready_generation="$(printf '%s' "$ready_index_out" | jq -r '.data.indexGeneration // empty')"
assert_nonempty "$ready_generation" "ready index exposes its published generation"
repeat_coalesce_out="$(ee_json --workspace "$WS" job run index_coalesce --item-limit 2 --json)"
assert_jq "$repeat_coalesce_out" '.schema == "ee.response.v2" and .success == true
    and .data.requestedJob == "index_coalesce"
    and .data.durableMutation == false
    and .data.summary == {"total":1,"succeeded":1,"skipped":0,"failed":0}
    and .data.job.outcome == "success"
    and .data.job.details.durableMutation == false
    and .data.job.details.result.status == "no_pending_jobs"
    and .data.job.details.result.pending_jobs == 0
    and .data.job.details.result.processed_jobs == 0
    and .data.job.details.result.completed_jobs == 0
    and .data.job.details.result.failed_jobs == 0
    and (.data.job.details.result.jobs | length == 0)' \
    "repeating public index_coalesce returns the exact no-pending idempotent result"
repeat_index_out="$(ee_json --workspace "$WS" index status --json)"
assert_jq "$repeat_index_out" ".data.health == \"ready\"
    and .data.dbGeneration == $ready_generation
    and .data.indexGeneration == $ready_generation
    and .data.indexDocumentCount == (2 + $spans_imported)" \
    "idempotent repeat preserves the exact ready generation and document count"

step "ambient capture suggest is read-only and proposes one explicit capture"
if capture_suggest_available; then
    before_suggest_candidates="$(candidate_count "$WS")"
    suggest_out="$(ee_json --workspace "$WS" capture suggest --from-session "$SESSION_PATH" --max 1 --min-confidence 0.45 --json)"
    assert_jq "$suggest_out" '.schema == "ee.response.v2" and .success == true' \
        "capture suggest succeeds for fixture session"
    assert_jq "$suggest_out" '
        .data.schema == "ee.capture_suggestions.v2"
        and .data.readOnly == true
        and .data.durableMutation == false
        and .data.candidateCount == 1
        and (.data.candidates | length) == 1
    ' "capture suggest returns exactly one read-only proposal"
    assert_jq "$suggest_out" '
        .data.candidates[0].acceptCommand | contains("ee curate")
    ' "capture suggest exposes an explicit accept command"
    assert_jq "$suggest_out" '
        .data.candidates[0].rejectCommand | contains("ee curate")
    ' "capture suggest exposes an explicit reject command"
    assert_jq "$suggest_out" ".data.candidates[0].acceptCommand | contains(\"--workspace\") and contains(\"$WS\")" \
        "capture suggest accept command preserves source workspace"
    assert_jq "$suggest_out" ".data.candidates[0].rejectCommand | contains(\"--workspace\") and contains(\"$WS\")" \
        "capture suggest reject command preserves source workspace"
    assert_jq "$suggest_out" ".data.nextAction | contains(\"--workspace\") and contains(\"$WS\")" \
        "capture suggest next action preserves source workspace"
    assert_jq "$suggest_out" '.data.nextAction | contains(" && ee curate accept ")' \
        "capture suggest next action is copy-pasteable shell"
    after_suggest_candidates="$(candidate_count "$WS")"
    assert_eq "$after_suggest_candidates" "$before_suggest_candidates" \
        "capture suggest does not persist curation candidates by itself"
else
    e2e_log_assert_eq "missing" "available" "capture suggest route available" || true
    _harness_fail "bd-2vq2z.7 capture suggest route is required: assert one above-threshold read-only proposal with explicit accept/reject commands"
fi

step "review session proposes linked session-arc candidates and accept is audited"
if review_session_available; then
    review_dry="$(ee_json --workspace "$WS" review session "$SESSION_PATH" --dry-run --limit 4 --min-confidence 0.35 --json)"
    assert_jq "$review_dry" '.schema == "ee.response.v2" and .success == true' \
        "review session dry-run succeeds"
    assert_jq "$review_dry" '
        .data.schema == "ee.review.session.v2"
        and .data.dryRun == true
        and .data.durableMutation == false
        and (.data.candidateCount // 0) >= 1
    ' "review session dry-run proposes without mutation"
    assert_jq "$review_dry" ".data.nextAction | contains(\"--workspace\") and contains(\"$WS\")" \
        "review session dry-run next action preserves source workspace"
    assert_jq "$review_dry" '.data.nextAction | contains("ee review session ")' \
        "review session dry-run next action is executable"
    before_review_candidates="$(candidate_count "$WS")"
    review_apply="$(ee_json --workspace "$WS" review session "$SESSION_PATH" --propose --limit 4 --min-confidence 0.35 --json)"
    assert_jq "$review_apply" '.schema == "ee.response.v2" and .success == true' \
        "review session propose succeeds"
    assert_jq "$review_apply" '
        .data.schema == "ee.review.session.v2"
        and .data.durableMutation == true
        and (.data.candidateCount // 0) >= 1
    ' "review session writes only curation proposals"
    assert_jq "$review_apply" ".data.nextAction | contains(\"--workspace\") and contains(\"$WS\")" \
        "review session propose next action preserves source workspace"
    assert_jq "$review_apply" '.data.nextAction | contains("ee curate candidates ")' \
        "review session propose next action lists pending candidates"
    after_review_candidates="$(candidate_count "$WS")"
    if [ "${after_review_candidates:-0}" -gt "${before_review_candidates:-0}" ]; then
        e2e_log_assert_eq "increased" "increased" "review session persisted proposal candidates" || true
        _harness_pass "review session persisted proposal candidates"
    else
        e2e_log_assert_eq "${after_review_candidates:-missing}" "greater-than-${before_review_candidates:-missing}" \
            "review session persisted proposal candidates" || true
        _harness_fail "review session did not increase curation candidates"
    fi

    anti_count="$(candidate_type_count "$review_apply" "anti-pattern|antipattern|failure")"
    rule_count="$(candidate_type_count "$review_apply" "rule|procedure|playbook")"
    if [ "${anti_count:-0}" -gt 0 ] && [ "${rule_count:-0}" -gt 0 ]; then
        e2e_log_assert_eq "linked-pair-present" "linked-pair-present" \
            "review session proposes anti-pattern plus rule pair" || true
        _harness_pass "review session proposes anti-pattern plus rule pair"
    else
        e2e_log_assert_eq "anti=${anti_count:-0},rule=${rule_count:-0}" "anti>0,rule>0" \
            "review session proposes anti-pattern plus rule pair" || true
        _harness_fail "bd-2vq2z.9 session-arc review must expose linked anti-pattern plus rule candidates"
    fi

    if curate_apply_available; then
        candidate_id="$(candidate_id_at "$review_apply" 0)"
        assert_nonempty "$candidate_id" "review session returns candidate id for explicit accept"
        if [ -n "$candidate_id" ] && [ "$candidate_id" != "null" ]; then
            accept_out="$(ee_json --workspace "$WS" curate apply "$candidate_id" --actor e2e_capture --json)"
            assert_jq "$accept_out" '.schema == "ee.response.v2" and .success == true' \
                "curate apply accepts the first review-session proposal"
            assert_jq "$accept_out" '
                (.data.application.status // .data.status // "") | test("applied|accepted|created"; "i")
            ' "accepted proposal is applied through curation"
            assert_audit_mentions_capture "$WS" "review-session accept"
        fi
    else
        e2e_log_assert_eq "missing" "available" "curate apply route available" || true
        _harness_fail "curate apply route is required to accept the first review-session candidate and assert curation audit"
    fi
else
    e2e_log_assert_eq "missing" "available" "review session route available" || true
    _harness_fail "bd-2vq2z.9 review session route is required: assert dry-run proposals, --propose curation writes, linked anti-pattern+rule pair, and audited accept"
fi

step "git commit capture proves dry-run default, apply gate, anchors, drift, and redaction"
(
    cd "$FIXTURE_REPO" || exit 1
    git init -q --initial-branch=main
    git config user.email "capture-e2e@example.invalid"
    git config user.name "Capture E2E"
    mkdir -p src
    cat >src/capture.rs <<'RS'
pub fn capture_lesson() -> &'static str {
    "route capture through explicit audit"
}
RS
    git add src/capture.rs
    git commit -q -m "capture: require explicit accept audit"
    cat >>src/capture.rs <<RS

pub fn redacted_secret_marker() -> &'static str {
    "$SECRET"
}
RS
    git add src/capture.rs
    git commit -q -m "capture: redact secret-bearing diff evidence"
)
if remember_git_capture_available; then
    fixture_init_out="$(ee_json --workspace "$FIXTURE_REPO" init --json)"
    assert_jq "$fixture_init_out" '.schema == "ee.response.v2" and .success == true' \
        "fixture git repo workspace init succeeds"
    before_git_memories="$(memory_list_count "$FIXTURE_REPO")"
    dry_commit="$(ee_json --workspace "$FIXTURE_REPO" remember --from-commit HEAD --json)"
    assert_jq "$dry_commit" '.schema == "ee.response.v2" and .success == true' \
        "remember --from-commit default dry-run succeeds"
    assert_jq "$dry_commit" '
        .data.command == "remember"
        and (.data.dry_run == true)
        and (.data.persisted == false)
        and (.data.content | contains("Mode: commit."))
        and (.data.content | contains("Diff fingerprint: blake3:"))
    ' "remember --from-commit defaults to no stored memory under dry-run"
    after_dry_git_memories="$(memory_list_count "$FIXTURE_REPO")"
    assert_eq "$after_dry_git_memories" "$before_git_memories" \
        "remember --from-commit dry-run does not mutate memory"

    apply_commit="$(ee_json --workspace "$FIXTURE_REPO" remember --from-commit HEAD --apply --json)"
    assert_jq "$apply_commit" '.schema == "ee.response.v2" and .success == true' \
        "remember --from-commit --apply succeeds"
    assert_jq "$apply_commit" '
        ((.data.persisted // false) == true)
        and ((.data.dry_run // true) == false)
        and ((.data.content // .data.memory.content // "") | test("capture: redact secret-bearing diff evidence|redact secret"; "i"))
    ' "git capture derives memory text from commit message"
    assert_jq "$apply_commit" '
        (.data.content | contains("src/capture.rs"))
        and (.data.content | contains("ee-anchor:path:src/capture.rs"))
        and (.data.content | test("ee-anchor:symbol:(capture_lesson|redacted_secret_marker)"))
        and (.data.content | contains("Diff fingerprint: blake3:"))
    ' "git capture includes file/symbol anchors and drift fingerprint"
    assert_jq "$apply_commit" "tostring | contains(\"$SECRET\") | not" \
        "git capture redacts secret-like diff evidence"
    assert_audit_mentions_capture "$FIXTURE_REPO" "from-commit apply"

    diff_dry="$(ee_json --workspace "$FIXTURE_REPO" remember --from-diff HEAD~1 --json)"
    assert_jq "$diff_dry" '.schema == "ee.response.v2" and .success == true' \
        "remember --from-diff default dry-run succeeds"
    assert_jq "$diff_dry" '
        .data.command == "remember"
        and (.data.persisted == false)
        and (.data.dry_run == true)
        and (.data.content | contains("Mode: diff."))
        and (.data.content | contains("Diff fingerprint: blake3:"))
    ' "remember --from-diff is proposal-only unless --apply is supplied"
else
    e2e_log_assert_eq "missing" "available" "remember git capture route available" || true
    _harness_fail "bd-2vq2z.8 remember --from-commit/--from-diff route is required: assert dry-run default, --apply persistence, file/symbol anchors, drift fingerprint, audit row, and secret redaction"
fi

step "rerun capture suggestion after acceptance suppresses duplicate takeover"
if capture_suggest_available; then
    rerun_out="$(ee_json --workspace "$WS" capture suggest --from-session "$SESSION_PATH" --max 2 --min-confidence 0.45 --json)"
    assert_jq "$rerun_out" '.schema == "ee.response.v2" and .success == true' \
        "capture suggest rerun succeeds"
    assert_jq "$rerun_out" '
        .data.schema == "ee.capture_suggestions.v2"
        and .data.readOnly == true
        and .data.durableMutation == false
        and ((.data.suppressedCount // 0) >= 1 or (.data.candidateCount // 0) <= 1)
    ' "rerun either suppresses duplicate suggestions or keeps one explicit proposal"
else
    e2e_log_assert_eq "missing" "available" "capture suggest rerun route available" || true
    _harness_fail "bd-2vq2z.7 capture suggest rerun is required: assert accepted lesson is not re-proposed as a takeover loop"
fi

end_temp_workspace
if [ -s "$EE_JSON_FAILURES_FILE" ]; then
    while IFS= read -r command_failure; do
        _harness_fail "logged command failure: $command_failure"
    done <"$EE_JSON_FAILURES_FILE"
fi
summary_rc=0
harness_summary || summary_rc=$?
printf 'Artifacts: %s\n' "$LOG_DIR" >&2
exit "$summary_rc"
