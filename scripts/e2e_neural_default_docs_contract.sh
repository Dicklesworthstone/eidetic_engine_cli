#!/usr/bin/env bash
# bd-1et0v.23 - neural-local default docs/degraded taxonomy contract.
#
# Static, no-Cargo E2E guard for the docs surfaces that describe the default
# embedding posture. It emits ee.test_event.v1 rows and validates the real
# repository files that central verification consumes.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_ID="neural_default_docs_contract"
RUN_ID="$(date -u +"%Y%m%dT%H%M%SZ").${BASHPID:-$$}"
ROOT_BASE="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
ARTIFACT_ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-neural-default-docs.XXXXXX")"
EVENT_LOG="${ARTIFACT_ROOT}/events.jsonl"
STDOUT_DIR="${ARTIFACT_ROOT}/stdout"
STDERR_DIR="${ARTIFACT_ROOT}/stderr"
FAILURES=0
BINARY_MATCHES_SOURCE=0

mkdir -p "$STDOUT_DIR" "$STDERR_DIR"

if ! command -v jq >/dev/null 2>&1; then
    printf '%s\n' '{"schema":"ee.test_event.v1","test_id":"neural_default_docs_contract","kind":"assert_result","fields":{"label":"jq_available","status":"fail","first_failure_diagnosis":"jq executable missing","stdout_artifact_path":"","stderr_artifact_path":"stderr","schema_validation_status":"not_run","redaction_status":"not_run"}}' >&2
    exit 1
fi

emit_event() {
    local kind="$1"
    local label="$2"
    local status="$3"
    local diagnosis="$4"
    local artifact_path="${5:-}"
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg test_id "$TEST_ID" \
        --arg kind "$kind" \
        --arg label "$label" \
        --arg status "$status" \
        --arg diagnosis "$diagnosis" \
        --arg artifact_path "$artifact_path" \
        '{schema:$schema,test_id:$test_id,kind:$kind,fields:{label:$label,status:$status,first_failure_diagnosis:$diagnosis,stdout_artifact_path:$artifact_path,stderr_artifact_path:"",schema_validation_status:"not_applicable",redaction_status:"passed"}}' \
        | tee -a "$EVENT_LOG" >&2
}

step() {
    local label="$1"
    local detail="$2"
    printf '[neural-default-docs][STEP] %s %s\n' "$label" "$detail" >&2
    jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg test_id "$TEST_ID" \
        --arg label "$label" \
        --arg detail "$detail" \
        '{schema:$schema,test_id:$test_id,kind:"step",fields:{label:$label,detail:$detail}}' \
        | tee -a "$EVENT_LOG" >&2
}

assert_file_contains() {
    local label="$1"
    local file="$2"
    local needle="$3"
    if grep -Fq "$needle" "$ROOT/$file"; then
        emit_event "assert_result" "$label" "pass" "none" "$file"
    else
        emit_event "assert_result" "$label" "fail" "missing expected text: $needle" "$file"
        FAILURES=$((FAILURES + 1))
    fi
}

assert_file_not_contains() {
    local label="$1"
    local file="$2"
    local needle="$3"
    if grep -Fq "$needle" "$ROOT/$file"; then
        emit_event "assert_result" "$label" "fail" "found forbidden stale text: $needle" "$file"
        FAILURES=$((FAILURES + 1))
    else
        emit_event "assert_result" "$label" "pass" "none" "$file"
    fi
}

assert_jq_file() {
    local label="$1"
    local file="$2"
    local filter="$3"
    local path
    case "$file" in
        /*) path="$file" ;;
        *) path="$ROOT/$file" ;;
    esac
    if jq -e "$filter" "$path" >/dev/null 2>&1; then
        emit_event "assert_result" "$label" "pass" "none" "$file"
    else
        emit_event "assert_result" "$label" "fail" "jq filter failed: $filter" "$file"
        FAILURES=$((FAILURES + 1))
    fi
}

resolve_ee_bin() {
    if [ -n "${EE_BIN:-}" ]; then
        printf '%s' "$EE_BIN"
        return 0
    fi
    if [ -n "${EE_BINARY:-}" ]; then
        printf '%s' "$EE_BINARY"
        return 0
    fi
    if command -v ee >/dev/null 2>&1; then
        command -v ee
        return 0
    fi
    printf ''
}

run_ee_json() {
    local label="$1"
    shift
    local stdout_file="$STDOUT_DIR/$label.json"
    local stderr_file="$STDERR_DIR/$label.txt"
    local rc=0
    step "command_start_$label" "$*"
    "$EE_BIN_RESOLVED" "$@" >"$stdout_file" 2>"$stderr_file" || rc=$?
    if [ "$rc" -eq 0 ]; then
        emit_event "command_end" "$label" "pass" "none" "$stdout_file"
    else
        emit_event "command_end" "$label" "fail" "command exited $rc" "$stderr_file"
        FAILURES=$((FAILURES + 1))
    fi
    LAST_STDOUT="$stdout_file"
}

step "harness_start" "run_id=$RUN_ID root=$ROOT artifacts=$ARTIFACT_ROOT"

CARGO_VERSION="$(awk -F' = ' '/^version = / {gsub(/"/, "", $2); print $2; exit}' "$ROOT/Cargo.toml")"
EE_BIN_RESOLVED="$(resolve_ee_bin)"
if [ -z "$EE_BIN_RESOLVED" ] || [ ! -x "$EE_BIN_RESOLVED" ]; then
    emit_event "assert_result" "ee_binary_available" "fail" "prebuilt ee binary missing; set EE_BIN or EE_BINARY" ""
    FAILURES=$((FAILURES + 1))
else
    emit_event "assert_result" "ee_binary_available" "pass" "none" "$EE_BIN_RESOLVED"
    step "real_binary_version" "assert ee binary version matches Cargo.toml"
    run_ee_json "version" version --json
    if jq -e --arg expected "$CARGO_VERSION" \
        '.schema == "ee.response.v2" and .success == true and .data.version == $expected' \
        "$LAST_STDOUT" >/dev/null 2>&1; then
        emit_event "assert_result" "ee_binary_matches_cargo_version" "pass" "none" "$LAST_STDOUT"
        BINARY_MATCHES_SOURCE=1
    else
        emit_event "assert_result" "ee_binary_matches_cargo_version" "fail" "binary is stale relative to Cargo.toml version $CARGO_VERSION" "$LAST_STDOUT"
        FAILURES=$((FAILURES + 1))
    fi
fi

step "readme_contract" "assert neural-local default is agent-visible"
assert_file_contains "readme_quick_example_neural_local" "README.md" "semantic backend: ready (neural_local, potion-multilingual-128M)"
assert_file_contains "readme_hybrid_row_neural_local" "README.md" "BM25 + neural-local vector search"
assert_file_contains "readme_agent_loop_default_hybrid" "README.md" 'ee pack "<task>" --workspace . --read-only --max-tokens 4000 --format markdown'
assert_file_not_contains "readme_agent_loop_no_lexical_only_default" "README.md" 'Starting substantive work | `ee pack "<task>" --workspace . --read-only --source-mode lexical_only'

step "agents_contract" "assert AGENTS feature flags match Cargo neural default"
assert_file_contains "agents_default_features_include_graph" "AGENTS.md" 'default = ["fts5", "json", "embed-fast", "lexical-bm25", "graph"]'
assert_file_contains "agents_embed_fast_includes_download" "AGENTS.md" 'embed-fast = ["frankensearch/model2vec", "frankensearch/download"]'
assert_file_contains "agents_local_first_mentions_auto_download" "AGENTS.md" "The pinned local Model2Vec embedding model may download automatically once"

step "taxonomy_contract" "assert embed_model_unavailable is model-load fallback"
assert_file_contains "taxonomy_bundled_model_load_failure" "docs/degraded_code_taxonomy.md" "the pinned bundled model cannot be loaded/downloaded"
assert_file_not_contains "taxonomy_no_old_dense_feature_trigger" "docs/degraded_code_taxonomy.md" "Build-time: no dense embedder feature compiled"
assert_file_contains "catalog_neural_local_trigger" "docs/degraded_codes.md" "default semantic path is neural-local via Frankensearch Model2Vec"

step "env_contract" "assert env knobs do not point operators at no-op model path"
assert_file_contains "env_foreign_embedding_presence_only" "docs/env_vars.md" "values are never displayed, never passed to Frankensearch, and never used for"
assert_file_contains "env_model_path_fault_injection" "docs/env_vars.md" "Not a user-facing model loader or model-selection knob"
assert_file_contains "env_embed_model_dir_real_loader" "docs/env_vars.md" 'Pre-populate this directory with `potion-multilingual-128M/`'

step "fixture_contract" "assert the locked failure fixture still names the public fault-injection route"
assert_jq_file "fixture_schema_valid" "tests/fixtures/failure_modes/embed_model_unavailable.json" '.schema == "ee.failure_mode_fixture.v1" and .code == "embed_model_unavailable"'
assert_jq_file "fixture_fault_injection_route" "tests/fixtures/failure_modes/embed_model_unavailable.json" '.trigger.invocation | contains("EE_EMBED_MODEL_PATH=/nonexistent/model")'
assert_jq_file "fixture_lexical_fallback_message" "tests/fixtures/failure_modes/embed_model_unavailable.json" '[.expected_emission.message_contains[]] | any(. == "lexical")'
assert_jq_file "fixture_neural_local_default_trigger" "tests/fixtures/failure_modes/embed_model_unavailable.json" '.trigger.description | contains("default semantic path is neural-local") and contains("potion-multilingual-128M") and (contains("default frankensearch_hash_fallback") | not)'

step "dependency_contract" "assert docs match Cargo feature shape"
assert_file_contains "feature_registry_download" "docs/feature_flag_registry.md" '["frankensearch/model2vec", "frankensearch/download"]'
assert_file_contains "dependency_matrix_download_allowed" "docs/dependency-contract-matrix.md" "the default download path is allowed because it uses asupersync HTTP"
assert_file_contains "dependency_research_mapping_download" "docs/dependency-research-notes.md" '`embed-fast` → `frankensearch/model2vec` + `frankensearch/download`'

if [ -n "$EE_BIN_RESOLVED" ] && [ -x "$EE_BIN_RESOLVED" ] && [ "$BINARY_MATCHES_SOURCE" -eq 1 ]; then
    step "real_binary_contract" "assert capabilities exposes EE_EMBED_MODEL_PATH as fault injection"
    run_ee_json "capabilities_full" capabilities --json --fields full
    assert_jq_file "capabilities_response_envelope" "$LAST_STDOUT" '.schema == "ee.response.v2" and .success == true'
    assert_jq_file "capabilities_model_path_fault_injection" "$LAST_STDOUT" '
        [.data.envOverrides[]? | select(.name == "EE_EMBED_MODEL_PATH") | .controls]
        | length == 1 and all(.[]; test("Fault-injection"))
    '
    assert_jq_file "capabilities_embed_download_default_auto" "$LAST_STDOUT" '
        [.data.envOverrides[]? | select(.name == "EE_EMBED_DOWNLOAD") | .defaultValue]
        | length == 1 and .[0] == "auto"
    '
fi

jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg test_id "$TEST_ID" \
    --arg artifact_root "$ARTIFACT_ROOT" \
    --arg event_log "$EVENT_LOG" \
    --argjson failures "$FAILURES" \
    '{schema:$schema,test_id:$test_id,kind:"summary",fields:{artifact_root:$artifact_root,event_log:$event_log,failures:$failures,status:(if $failures == 0 then "pass" else "fail" end)}}' \
    | tee -a "$EVENT_LOG" >&2

printf 'neural default docs contract artifacts: %s\n' "$ARTIFACT_ROOT" >&2
if [ "$FAILURES" -ne 0 ]; then
    exit 1
fi
