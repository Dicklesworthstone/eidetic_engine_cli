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

ee_json() {
    e2e_log_command "$EE_BIN" "$@" || true
}

ee_json_with_env() {
    local env_key="$1"
    local env_value="$2"
    shift 2
    e2e_log_command env "$env_key=$env_value" "$EE_BIN" "$@" || true
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
    cat >"$path" <<FAKECASS
#!/bin/sh
set -eu
cmd="\${1:-}"
case "\$cmd" in
  sessions)
    cat <<'JSON'
{"sessions":[{"path":"$session_path","workspace":"$WS","agent":"codex","started_at":"2026-06-17T13:00:00Z","ended_at":"2026-06-17T13:20:00Z","message_count":6,"token_count":920,"content_hash":"blake3:capture-e2e-session"}]}
JSON
    ;;
  view)
    cat <<'JSON'
{"path":"$session_path","target_line":2,"context":3,"lines":[
  {"line":1,"content":"{\"role\":\"user\",\"content\":\"The capture workflow kept reproposing the same lesson after acceptance.\"}","highlighted":false},
  {"line":2,"content":"{\"role\":\"assistant\",\"content\":\"Lesson: ambient capture must dedupe accepted suggestions and route storage through explicit curation accept.\"}","highlighted":true},
  {"line":3,"content":"{\"role\":\"assistant\",\"content\":\"Failure arc: storing silently would violate the no-loop-takeover policy.\"}","highlighted":false},
  {"line":4,"content":"{\"role\":\"user\",\"content\":\"Fix: require accept/reject commands and audit every accepted capture.\"}","highlighted":false}
],"total_lines":4}
JSON
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
write_fake_cass_binary "$CASS_BIN" "$SESSION_PATH"
log_event "capture_fixture_ready" \
    bead "bd-2vq2z.20" \
    workspace "$WS" \
    fixtureRepo "$FIXTURE_REPO" \
    cassSession "$SESSION_PATH"

step "import fixture CASS session without silent memory mutation"
before_import_memories="$(memory_list_count "$WS")"
import_out="$(ee_json_with_env EE_CASS_BINARY "$CASS_BIN" --workspace "$WS" import cass --limit 1 --json)"
assert_jq "$import_out" '.schema == "ee.response.v2" and .success == true' \
    "fixture cass import succeeds"
assert_jq "$import_out" '
    (.data.schema == "ee.import.cass.v1")
    and ((.data.sessionsImported // .data.sessions_imported // 0) >= 1)
    and ((.data.spansImported // .data.spans_imported // 0) >= 1)
' "fixture cass import stores one session with evidence spans"
after_import_memories="$(memory_list_count "$WS")"
assert_zero "$after_import_memories" \
    "cass import creates evidence but does not silently store memories"
assert_zero "$before_import_memories" \
    "capture workspace starts without memories"

step "ambient capture suggest is read-only and proposes one explicit capture"
if capture_suggest_available; then
    before_suggest_candidates="$(candidate_count "$WS")"
    suggest_out="$(ee_json --workspace "$WS" capture suggest --from-session "$SESSION_PATH" --max 1 --min-confidence 0.45 --json)"
    assert_jq "$suggest_out" '.schema == "ee.response.v2" and .success == true' \
        "capture suggest succeeds for fixture session"
    assert_jq "$suggest_out" '
        .data.schema == "ee.capture_suggestions.v1"
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
        .data.schema == "ee.review.session.v1"
        and .data.dryRun == true
        and .data.durableMutation == false
        and (.data.candidateCount // 0) >= 1
    ' "review session dry-run proposes without mutation"
    before_review_candidates="$(candidate_count "$WS")"
    review_apply="$(ee_json --workspace "$WS" review session "$SESSION_PATH" --propose --limit 4 --min-confidence 0.35 --json)"
    assert_jq "$review_apply" '.schema == "ee.response.v2" and .success == true' \
        "review session propose succeeds"
    assert_jq "$review_apply" '
        .data.schema == "ee.review.session.v1"
        and .data.durableMutation == true
        and (.data.candidateCount // 0) >= 1
    ' "review session writes only curation proposals"
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
        .data.schema == "ee.capture_suggestions.v1"
        and .data.readOnly == true
        and .data.durableMutation == false
        and ((.data.suppressedCount // 0) >= 1 or (.data.candidateCount // 0) <= 1)
    ' "rerun either suppresses duplicate suggestions or keeps one explicit proposal"
else
    e2e_log_assert_eq "missing" "available" "capture suggest rerun route available" || true
    _harness_fail "bd-2vq2z.7 capture suggest rerun is required: assert accepted lesson is not re-proposed as a takeover loop"
fi

end_temp_workspace
summary_rc=0
harness_summary || summary_rc=$?
printf 'Artifacts: %s\n' "$LOG_DIR" >&2
exit "$summary_rc"
