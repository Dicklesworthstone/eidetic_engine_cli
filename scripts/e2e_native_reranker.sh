#!/usr/bin/env bash
# bd-1nl13.14 - native reranker real-binary E2E.
#
# This lane is intentionally no-Cargo: scripts/verify.sh owns compilation and
# supplies a real ee binary through EE_BINARY/EE_BIN or PATH. The harness:
#   - proves missing and rejected-corrupt artifacts degrade to fusion-only with
#     a successful search exit and explicit rerank_model_unavailable evidence;
#   - provisions the verified safetensors archive through the public model
#     command, proves doctor observes recovery, and proves a real cross-encoder
#     changes the fusion-only order;
#   - rejects any ee binary dynamically linked to ONNX Runtime / libort; and
#   - retains ee.test_event.v1 command, assertion, timing, and artifact logs.
#
# Model-backed execution is automatic when the verified reranker archive is
# available. Set EE_E2E_NATIVE_RERANK_REQUIRE_MODEL=1 to make absence fail
# closed. Embedding assets are intentionally unnecessary: production applies
# the native reranker directly to deterministic Phase 1 candidates.
# Artifacts are retained deliberately; this script never removes user data.

set -uo pipefail

TEST_ID="native_reranker_e2e"
BEAD_ID="bd-1nl13.14"
SEARCH_QUERY="cargo fmt check and cargo clippy before publishing"
RERANK_VECTOR_OUT="${EE_E2E_RERANK_VECTOR_OUT:-}"
TARGET_TRIPLE="${EE_E2E_TARGET_TRIPLE:-unspecified}"

if ! command -v jq >/dev/null 2>&1; then
    printf '{"schema":"ee.test_event.v1","ts":"%s","test_id":"native_reranker_e2e","kind":"assert_fail","fields":{"bead_id":"bd-1nl13.14","label":"jq_available","expected":"jq executable on PATH","actual":"missing","schema_validation_status":"not_run","redaction_status":"passed","first_failure_diagnosis":"jq executable missing before harness initialization","stdout_artifact_path":"not_initialized","stderr_artifact_path":"stderr","sanitized_env":{"HOME":"[UNREAD]"}}}\n' \
        "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" >&2
    exit 3
fi

emit_assert_fail_with_artifact_paths() {
    local label="${1:?label required}"
    local expected="${2:?expected required}"
    local actual="${3:?actual required}"
    local diagnosis="${4:?diagnosis required}"
    jq -cn \
        --arg ts "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
        --arg label "${label}" --arg expected "${expected}" \
        --arg actual "${actual}" --arg diagnosis "${diagnosis}" \
        '{schema:"ee.test_event.v1",ts:$ts,test_id:"native_reranker_e2e",kind:"assert_fail",fields:{bead_id:"bd-1nl13.14",label:$label,expected:$expected,actual:$actual,schema_validation_status:"not_run",redaction_status:"passed",first_failure_diagnosis:$diagnosis,stdout_artifact_path:"not_initialized",stderr_artifact_path:"stderr",sanitized_env:{HOME:"[UNREAD]"}}}' >&2
}

PYTHON_COMMAND=""
if command -v python3 >/dev/null 2>&1; then
    PYTHON_COMMAND="python3"
elif command -v python >/dev/null 2>&1; then
    PYTHON_COMMAND="python"
else
    emit_assert_fail_with_artifact_paths "python3_available" \
        "python3 or python executable on PATH" "missing" \
        "Python is required for monotonic millisecond timing"
    exit 3
fi

EE_COMMAND="${EE_BIN:-${EE_BINARY:-ee}}"
if [[ "${EE_COMMAND}" == */* ]]; then
    REAL_EE="${EE_COMMAND}"
else
    REAL_EE="$(command -v "${EE_COMMAND}" 2>/dev/null || true)"
fi
if [[ -z "${REAL_EE}" || ! -x "${REAL_EE}" ]]; then
    emit_assert_fail_with_artifact_paths "ee_binary_available" \
        "executable prebuilt ee binary" "missing or non-executable" \
        "prebuilt ee binary missing; the E2E lane never invokes Cargo"
    exit 3
fi

ROOT_BASE="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
if [[ ! -d "${ROOT_BASE}" || ! -w "${ROOT_BASE}" ]]; then
    emit_assert_fail_with_artifact_paths "artifact_root_available" \
        "existing writable directory" "${ROOT_BASE}" \
        "EE_E2E_TMPDIR/TMPDIR artifact root is unavailable or unwritable"
    exit 3
fi
if ! ROOT="$(mktemp -d "${ROOT_BASE%/}/ee-native-reranker-e2e.XXXXXX")"; then
    emit_assert_fail_with_artifact_paths "artifact_root_created" \
        "isolated retained artifact directory" "mktemp failed" \
        "failed to create native-reranker E2E artifact root"
    exit 3
fi
LOG_DIR="${ROOT}/logs"
EVENT_LOG="${LOG_DIR}/events.jsonl"
EMPTY_EMBED_MODEL_DIR="${ROOT}/empty-embedding-model"
if ! mkdir -p "${LOG_DIR}" "${EMPTY_EMBED_MODEL_DIR}" \
    || ! : >"${EVENT_LOG}"; then
    emit_assert_fail_with_artifact_paths "event_log_initialized" \
        "writable retained event log" "initialization failed" \
        "failed to initialize native-reranker E2E event log"
    exit 3
fi

FAILURES=0
PASSES=0
SKIPS=0
STEP=0
FINISHED=0
LOGGING_FAILURES=0
LAST_STDOUT_FILE=""
VECTOR_EMITTED=0

now_iso() {
    date -u +"%Y-%m-%dT%H:%M:%SZ"
}

now_ms() {
    "${PYTHON_COMMAND}" -c 'import time; print(int(time.time() * 1000))'
}

native_path() {
    local path="${1:?path required}"
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -w "${path}"
    else
        printf '%s' "${path}"
    fi
}

hash_args() {
    if command -v shasum >/dev/null 2>&1; then
        printf '%s\0' "$@" | shasum -a 256 | awk '{print "sha256:" $1}'
    else
        printf '%s\0' "$@" | sha256sum | awk '{print "sha256:" $1}'
    fi
}

hash_file() {
    local file="${1:?file required}"
    if command -v b3sum >/dev/null 2>&1; then
        printf 'blake3:%s' "$(b3sum "${file}" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        printf 'sha256:%s' "$(shasum -a 256 "${file}" | awk '{print $1}')"
    else
        printf 'sha256:%s' "$(sha256sum "${file}" | awk '{print $1}')"
    fi
}

sha256_file() {
    local file="${1:?file required}"
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "${file}" | awk '{print $1}'
    else
        sha256sum "${file}" | awk '{print $1}'
    fi
}

emit_event() {
    local kind="${1:?kind required}"
    local fields_json="${2:-}"
    local event_error_status=1
    local event_json
    if [[ -z "${fields_json}" ]]; then
        fields_json='{}'
    fi
    if ! event_json="$(jq -cn \
        --arg schema "ee.test_event.v1" \
        --arg ts "$(now_iso)" \
        --arg testId "${TEST_ID}" \
        --arg kind "${kind}" \
        --argjson fields "${fields_json}" \
        '{schema:$schema,ts:$ts,test_id:$testId,kind:$kind,fields:$fields}')"; then
        LOGGING_FAILURES=$((LOGGING_FAILURES + 1))
        printf 'native_reranker_e2e: failed to serialize event kind=%s\n' \
            "${kind}" >&2
        return "${event_error_status}"
    fi
    if ! printf '%s\n' "${event_json}" >>"${EVENT_LOG}"; then
        LOGGING_FAILURES=$((LOGGING_FAILURES + 1))
        printf 'native_reranker_e2e: failed to append event kind=%s log=%s\n' \
            "${kind}" "${EVENT_LOG}" >&2
        return "${event_error_status}"
    fi
    return 0
}

record_pass() {
    local label="${1:?label required}"
    PASSES=$((PASSES + 1))
    emit_event "assert_ok" "$(jq -cn \
        --arg bead "${BEAD_ID}" --arg label "${label}" \
        '{bead_id:$bead,surface:"native_reranker_e2e",label:$label,schema_validation_status:"passed",redaction_status:"passed",sanitized_env:{HOME:"[ISOLATED_HOME]",NO_COLOR:"1",EE_EMBED_DOWNLOAD:"off",FRANKENSEARCH_OFFLINE:"1",FRANKENSEARCH_ALLOW_DOWNLOAD:"0"}}')"
    printf '  [PASS] %s\n' "${label}" >&2
}

record_failure() {
    local label="${1:?label required}"
    local detail="${2:-failed}"
    FAILURES=$((FAILURES + 1))
    emit_event "assert_fail" "$(jq -cn \
        --arg bead "${BEAD_ID}" --arg label "${label}" --arg detail "${detail}" \
        '{bead_id:$bead,surface:"native_reranker_e2e",label:$label,expected:"assertion satisfied",actual:$detail,detail:$detail,schema_validation_status:"failed",first_failure_diagnosis:$detail,stdout_artifact_path:"see preceding command_end or not_applicable",stderr_artifact_path:"see preceding command_end or stderr",redaction_status:"passed",sanitized_env:{HOME:"[ISOLATED_HOME]",NO_COLOR:"1",EE_EMBED_DOWNLOAD:"off",FRANKENSEARCH_OFFLINE:"1",FRANKENSEARCH_ALLOW_DOWNLOAD:"0"}}')"
    printf '  [FAIL] %s: %s\n' "${label}" "${detail}" >&2
}

record_skip() {
    local label="${1:?label required}"
    local detail="${2:?detail required}"
    SKIPS=$((SKIPS + 1))
    emit_event "note" "$(jq -cn \
        --arg bead "${BEAD_ID}" --arg label "${label}" --arg detail "${detail}" \
        '{bead_id:$bead,surface:"native_reranker_e2e",label:"lane_skip",lane:$label,detail:$detail,schema_validation_status:"not_applicable",redaction_status:"passed",sanitized_env:{HOME:"[ISOLATED_HOME]"}}')"
    printf '  [SKIP] %s: %s\n' "${label}" "${detail}" >&2
}

assert_json_file() {
    local file="${1:?file required}"
    local filter="${2:?jq filter required}"
    local label="${3:?label required}"
    if jq -e "${filter}" "${file}" >/dev/null 2>&1; then
        record_pass "${label}"
    else
        record_failure "${label}" "jq filter failed: ${filter}; artifact=${file}"
    fi
}

assert_eq() {
    local actual="${1-}"
    local expected="${2-}"
    local label="${3:?label required}"
    if [[ "${actual}" == "${expected}" ]]; then
        record_pass "${label}"
    else
        record_failure "${label}" "expected=${expected} actual=${actual}"
    fi
}

run_ee_json() {
    local label="${1:?label required}"
    local expected_exit="${2:?expected exit required}"
    local home_dir="${3:?home dir required}"
    local workspace="${4:?workspace required}"
    shift 4

    STEP=$((STEP + 1))
    local stdout_file stderr_file
    local argv_hash arg_count start_ms end_ms elapsed_ms exit_code
    local native_home native_root native_workspace native_empty_embed_model
    local native_appdata native_localappdata system_root windows_dir comspec
    local -a command_env
    stdout_file="${LOG_DIR}/step_$(printf '%02d' "${STEP}")_${label//[^A-Za-z0-9_]/_}.stdout.json"
    stderr_file="${LOG_DIR}/step_$(printf '%02d' "${STEP}")_${label//[^A-Za-z0-9_]/_}.stderr.txt"
    argv_hash="$(hash_args "$@")"
    arg_count=$#
    native_home="$(native_path "${home_dir}")"
    native_root="$(native_path "${ROOT}")"
    native_workspace="$(native_path "${workspace}")"
    native_empty_embed_model="$(native_path "${EMPTY_EMBED_MODEL_DIR}")"
    if ! mkdir -p "${home_dir}/AppData/Roaming" "${home_dir}/AppData/Local"; then
        record_failure "isolated Windows application-data roots are writable" \
            "failed to create application-data roots below ${home_dir}"
        return 1
    fi
    native_appdata="$(native_path "${home_dir}/AppData/Roaming")"
    native_localappdata="$(native_path "${home_dir}/AppData/Local")"
    system_root="${SYSTEMROOT:-${SystemRoot:-}}"
    windows_dir="${WINDIR:-${windir:-}}"
    comspec="${COMSPEC:-${ComSpec:-}}"
    command_env=(
        env -i
        "HOME=${native_home}"
        "USERPROFILE=${native_home}"
        "APPDATA=${native_appdata}"
        "LOCALAPPDATA=${native_localappdata}"
        "PATH=${PATH:-/usr/bin:/bin}"
        "TMPDIR=${native_root}"
        "TMP=${native_root}"
        "TEMP=${native_root}"
        "NO_COLOR=1"
        "EE_EMBED_DOWNLOAD=off"
        "EE_EMBED_MODEL_DIR=${native_empty_embed_model}"
        "FRANKENSEARCH_OFFLINE=1"
        "FRANKENSEARCH_ALLOW_DOWNLOAD=0"
    )
    if [[ -n "${system_root}" ]]; then
        command_env+=("SYSTEMROOT=${system_root}")
    fi
    if [[ -n "${windows_dir}" ]]; then
        command_env+=("WINDIR=${windows_dir}")
    fi
    if [[ -n "${comspec}" ]]; then
        command_env+=("COMSPEC=${comspec}")
    fi
    printf '[%02d] %s\n' "${STEP}" "${label}" >&2
    emit_event "command_start" "$(jq -cn \
        --arg bead "${BEAD_ID}" --arg label "${label}" \
        --arg argvHash "${argv_hash}" --argjson argCount "${arg_count}" \
        '{bead_id:$bead,surface:"native_reranker_e2e",label:$label,command:"ee",arg_count:$argCount,argv_hash:$argvHash,workspace:"[RUN_WORKSPACE]",cwd:"[REPO_ROOT]",schema_validation_status:"pending",redaction_status:"passed",sanitized_env:{HOME:"[ISOLATED_HOME]",NO_COLOR:"1",EE_EMBED_DOWNLOAD:"off",EE_EMBED_MODEL_DIR:"[EMPTY_MODEL_DIR]",FRANKENSEARCH_OFFLINE:"1",FRANKENSEARCH_ALLOW_DOWNLOAD:"0"}}')"

    start_ms="$(now_ms)"
    "${command_env[@]}" "${REAL_EE}" --workspace "${native_workspace}" "$@" \
        >"${stdout_file}" 2>"${stderr_file}"
    exit_code=$?
    end_ms="$(now_ms)"
    elapsed_ms=$((end_ms - start_ms))

    emit_event "command_end" "$(jq -cn \
        --arg bead "${BEAD_ID}" --arg label "${label}" \
        --arg stdout "${stdout_file}" --arg stderr "${stderr_file}" \
        --arg stdoutHash "$(hash_file "${stdout_file}")" \
        --arg stderrHash "$(hash_file "${stderr_file}")" \
        --arg argvHash "${argv_hash}" --argjson argCount "${arg_count}" \
        --argjson exitCode "${exit_code}" \
        --argjson elapsedMs "${elapsed_ms}" \
        '{bead_id:$bead,surface:"native_reranker_e2e",label:$label,command:"ee",arg_count:$argCount,argv_hash:$argvHash,exit_code:$exitCode,elapsed_ms:$elapsedMs,stdout_artifact_path:$stdout,stderr_artifact_path:$stderr,stdout_hash:$stdoutHash,stderr_hash:$stderrHash,schema_validation_status:"json_checked_below",redaction_status:"passed",sanitized_env:{HOME:"[ISOLATED_HOME]",NO_COLOR:"1",EE_EMBED_DOWNLOAD:"off",EE_EMBED_MODEL_DIR:"[EMPTY_MODEL_DIR]",FRANKENSEARCH_OFFLINE:"1",FRANKENSEARCH_ALLOW_DOWNLOAD:"0"}}')"

    LAST_STDOUT_FILE="${stdout_file}"

    if [[ "${exit_code}" -eq "${expected_exit}" ]]; then
        record_pass "${label} exit ${expected_exit}"
    else
        record_failure "${label} exit ${expected_exit}" \
            "actual_exit=${exit_code} stdout=${stdout_file} stderr=${stderr_file}"
    fi
    if jq -e 'type == "object"' "${stdout_file}" >/dev/null 2>&1; then
        record_pass "${label} stdout is JSON object"
    else
        record_failure "${label} stdout is JSON object" \
            "invalid JSON stdout=${stdout_file} stderr=${stderr_file}"
    fi
}

init_workspace() {
    local home_dir="${1:?home dir required}"
    local workspace="${2:?workspace required}"
    mkdir -p "${home_dir}" "${workspace}"
    run_ee_json "init_workspace" 0 "${home_dir}" "${workspace}" init --json
    assert_json_file "${LAST_STDOUT_FILE}" \
        '.schema == "ee.response.v2" and .success == true' \
        "workspace init response contract"
}

seed_rerank_corpus() {
    local home_dir="${1:?home dir required}"
    local workspace="${2:?workspace required}"
    local label content level kind

    while IFS='|' read -r label level kind content; do
        run_ee_json "remember_${label}" 0 "${home_dir}" "${workspace}" \
            remember "${content}" --level "${level}" --kind "${kind}" \
            --tags "${BEAD_ID},rerank,e2e" --no-auto-link \
            --no-propose-candidates --json
        assert_json_file "${LAST_STDOUT_FILE}" '.success == true' \
            "${label} memory stored"
    done <<'EOF'
trap|semantic|fact|BD1NL1314_RERANK_TRAP release release release format format checklist checklist cargo cargo clippy clippy, but this is a noisy lexical trap and not the Rust release policy target.
target|procedural|rule|BD1NL1314_RERANK_TARGET The correct Rust release policy says run cargo fmt --check and cargo clippy before publishing.
noise_one|semantic|fact|BD1NL1314_RERANK_NOISE_ONE Database migration notes cover index ownership and schema upgrade ordering.
noise_two|semantic|fact|BD1NL1314_RERANK_NOISE_TWO Onboarding screenshots and terminal color themes need a design review.
noise_three|semantic|fact|BD1NL1314_RERANK_NOISE_THREE Rust ownership and borrowing prevent memory safety errors at compile time.
EOF

    run_ee_json "index_rebuild" 0 "${home_dir}" "${workspace}" index rebuild --json
    assert_json_file "${LAST_STDOUT_FILE}" \
        '.success == true and (.data.documents_total // .data.documentsTotal // 0) >= 5' \
        "rerank fixture index contains five documents"
}

inspect_binary_linkage() {
    STEP=$((STEP + 1))
    local stdout_file stderr_file
    local tool="" exit_code=0 start_ms end_ms elapsed_ms argv_hash arg_count os_name
    local -a command=()
    stdout_file="${LOG_DIR}/step_$(printf '%02d' "${STEP}")_binary_linkage.stdout.txt"
    stderr_file="${LOG_DIR}/step_$(printf '%02d' "${STEP}")_binary_linkage.stderr.txt"

    os_name="$(uname -s)"
    case "${os_name}" in
        Darwin*)
            if command -v otool >/dev/null 2>&1; then
                tool="otool"
                command=(otool -L "${REAL_EE}")
            fi
            ;;
        MINGW* | MSYS* | CYGWIN*)
            if command -v dumpbin.exe >/dev/null 2>&1; then
                tool="dumpbin"
                command=(dumpbin.exe /DEPENDENTS "$(native_path "${REAL_EE}")")
            elif command -v llvm-objdump.exe >/dev/null 2>&1; then
                tool="llvm-objdump"
                command=(llvm-objdump.exe -p "$(native_path "${REAL_EE}")")
            elif command -v llvm-objdump >/dev/null 2>&1; then
                tool="llvm-objdump"
                command=(llvm-objdump -p "$(native_path "${REAL_EE}")")
            elif command -v objdump.exe >/dev/null 2>&1; then
                tool="objdump"
                command=(objdump.exe -p "$(native_path "${REAL_EE}")")
            elif command -v objdump >/dev/null 2>&1; then
                tool="objdump"
                command=(objdump -p "$(native_path "${REAL_EE}")")
            fi
            ;;
        *)
            if command -v ldd >/dev/null 2>&1; then
                tool="ldd"
                command=(ldd "${REAL_EE}")
            elif command -v objdump >/dev/null 2>&1; then
                tool="objdump"
                command=(objdump -p "${REAL_EE}")
            fi
            ;;
    esac
    if [[ -z "${tool}" ]]; then
        record_failure "native linkage inspection tool available" \
            "none of otool, ldd, llvm-objdump, objdump, or dumpbin is available"
        return
    fi

    printf '[%02d] inspect binary linkage with %s\n' "${STEP}" "${tool}" >&2
    argv_hash="$(hash_args "${command[@]:1}")"
    arg_count=$((${#command[@]} - 1))
    emit_event "command_start" "$(jq -cn \
        --arg bead "${BEAD_ID}" --arg tool "${tool}" \
        --arg argvHash "${argv_hash}" --argjson argCount "${arg_count}" \
        '{bead_id:$bead,surface:"native_reranker_e2e",label:"binary_linkage",command:$tool,arg_count:$argCount,argv_hash:$argvHash,cwd:"[REPO_ROOT]",schema_validation_status:"not_applicable",redaction_status:"passed",sanitized_env:{HOME:"[ORIGINAL_HOME]"}}')"
    start_ms="$(now_ms)"
    if [[ "${tool}" == "dumpbin" ]]; then
        MSYS2_ARG_CONV_EXCL='*' "${command[@]}" \
            >"${stdout_file}" 2>"${stderr_file}"
    else
        "${command[@]}" >"${stdout_file}" 2>"${stderr_file}"
    fi
    exit_code=$?
    end_ms="$(now_ms)"
    elapsed_ms=$((end_ms - start_ms))
    emit_event "command_end" "$(jq -cn \
        --arg bead "${BEAD_ID}" --arg tool "${tool}" \
        --arg stdout "${stdout_file}" --arg stderr "${stderr_file}" \
        --arg stdoutHash "$(hash_file "${stdout_file}")" \
        --arg stderrHash "$(hash_file "${stderr_file}")" \
        --arg argvHash "${argv_hash}" --argjson argCount "${arg_count}" \
        --argjson exitCode "${exit_code}" \
        --argjson elapsedMs "${elapsed_ms}" \
        '{bead_id:$bead,surface:"native_reranker_e2e",label:"binary_linkage",command:$tool,arg_count:$argCount,argv_hash:$argvHash,exit_code:$exitCode,elapsed_ms:$elapsedMs,stdout_artifact_path:$stdout,stderr_artifact_path:$stderr,stdout_hash:$stdoutHash,stderr_hash:$stderrHash,schema_validation_status:"not_applicable",redaction_status:"passed",sanitized_env:{HOME:"[ORIGINAL_HOME]"}}')"

    if [[ "${exit_code}" -ne 0 ]]; then
        if [[ "${tool}" == "ldd" ]] \
            && grep -Eiq 'not a dynamic executable|statically linked' \
                "${stdout_file}" "${stderr_file}"; then
            record_pass "static ee binary requires no ONNX runtime linkage"
        else
            record_failure "binary linkage command succeeds" \
                "${tool} exited ${exit_code}; stdout=${stdout_file} stderr=${stderr_file}"
            return
        fi
    else
        record_pass "binary linkage command succeeds"
    fi

    if grep -Eiq 'libonnxruntime|onnxruntime|libort([.-]|$)|(^|[[:space:]\\/])ort\.dll($|[[:space:]])' \
        "${stdout_file}" "${stderr_file}"; then
        record_failure "ee binary is ORT-free" \
            "forbidden ONNX Runtime linkage found in ${stdout_file}"
    else
        record_pass "ee binary is ORT-free"
    fi
}

validate_rerank_vector() {
    local vector="${1:?vector required}"
    jq -e --arg query "${SEARCH_QUERY}" '
        (keys | sort) == [
            "fusionOnlyOrder",
            "query",
            "rerankedOrder",
            "rerankedScores",
            "schema"
        ]
        and .schema == "ee.rerank_determinism.vector.v1"
        and .query == $query
        and (.fusionOnlyOrder | length) == 5
        and all(.fusionOnlyOrder[]; type == "string")
        and (.rerankedOrder | length) == 5
        and all(.rerankedOrder[]; type == "string")
        and (.rerankedScores | length) == 5
        and [.rerankedScores[].content] == .rerankedOrder
        and all(.rerankedScores[];
            (.rerankScore | type) == "number"
            and .rerankScore >= 0
            and .rerankScore <= 1)
    ' "${vector}" >/dev/null 2>&1
}

write_rerank_vector() {
    local fusion_file="${1:?fusion result required}"
    local reranked_file="${2:?reranked result required}"
    local vector_parent

    if [[ -z "${RERANK_VECTOR_OUT}" ]]; then
        return
    fi
    vector_parent="$(dirname "${RERANK_VECTOR_OUT}")"
    if ! mkdir -p "${vector_parent}"; then
        record_failure "reranker determinism vector parent is writable" \
            "failed to create ${vector_parent}"
        return
    fi
    if ! jq -S \
        --arg query "${SEARCH_QUERY}" \
        --argjson fusionOrder "$(jq -c '[.data.results[].content]' "${fusion_file}")" \
        '{
            schema: "ee.rerank_determinism.vector.v1",
            query: $query,
            fusionOnlyOrder: $fusionOrder,
            rerankedOrder: [.data.results[].content],
            rerankedScores: [.data.results[] | {content, rerankScore}]
        }' "${reranked_file}" >"${RERANK_VECTOR_OUT}"; then
        record_failure "reranker determinism vector is emitted" \
            "jq failed while writing ${RERANK_VECTOR_OUT}"
        return
    fi
    if validate_rerank_vector "${RERANK_VECTOR_OUT}"; then
        VECTOR_EMITTED=1
        record_pass "reranker determinism vector is emitted"
        emit_event "note" "$(jq -cn \
            --arg bead "${BEAD_ID}" --arg target "${TARGET_TRIPLE}" \
            --arg vector "${RERANK_VECTOR_OUT}" \
            '{bead_id:$bead,surface:"native_reranker_e2e",label:"rerank_determinism_vector",target:$target,artifact_path:$vector,schema_validation_status:"passed",redaction_status:"passed",sanitized_env:{HOME:"[ISOLATED_HOME]"}}')"
    else
        record_failure "reranker determinism vector is emitted" \
            "invalid vector contract: ${RERANK_VECTOR_OUT}"
    fi
}

run_degradation_lane() {
    local home_dir="${ROOT}/degraded-home"
    local workspace="${ROOT}/degraded-workspace"
    local corrupt_archive="${ROOT}/corrupt-rerank-default-v1.tar.zst"
    local initial_order replay_order

    init_workspace "${home_dir}" "${workspace}"
    seed_rerank_corpus "${home_dir}" "${workspace}"

    run_ee_json "missing_model_search" 0 "${home_dir}" "${workspace}" \
        search "${SEARCH_QUERY}" \
        --limit 5 --relevance-floor 0 --explain --json
    assert_json_file "${LAST_STDOUT_FILE}" '
        .schema == "ee.response.v2"
        and .success == true
        and .data.rerank.schema == "ee.rerank_posture.v1"
        and .data.rerank.mode == "fusion_only_degraded"
        and .data.rerank.available == false
        and .data.rerank.degradedCode == "rerank_model_unavailable"
        and .data.rerank.rerankScoreCount == 0
        and all(.data.results[]; .scoreKind != "reranked" and (has("rerankScore") | not))
        and .data.rerank.advisory == {
            "code": "rerank_model_unavailable",
            "severity": "low",
            "permanent": true,
            "message": "No usable local reranker is registered. Search is using fusion-only ranking. Network download is unavailable, but a verified offline reranker artifact can be imported explicitly.",
            "repair": null,
            "resolution": "automatic_repair_unavailable"
        }
        and all(.data.degraded[]; .code != "rerank_model_unavailable")
    ' "missing model degrades explicitly to fusion-only"
    initial_order="$(jq -c '[.data.results[].memoryId]' "${LAST_STDOUT_FILE}" 2>/dev/null || printf '[]')"

    printf '%s\n' 'deliberately invalid rerank archive for bd-1nl13.14' \
        >"${corrupt_archive}"
    run_ee_json "corrupt_model_fetch" 2 "${home_dir}" "${workspace}" \
        model fetch rerank-default --from-file "${corrupt_archive}" --json
    assert_json_file "${LAST_STDOUT_FILE}" '
        .schema == "ee.error.v2"
        and .error.code == "configuration"
        and (.error.message | test("length mismatch|hash mismatch"; "i"))
    ' "corrupt artifact is rejected with configuration error"

    run_ee_json "post_corrupt_fallback_search" 0 "${home_dir}" "${workspace}" \
        search "${SEARCH_QUERY}" \
        --limit 5 --relevance-floor 0 --explain --json
    assert_json_file "${LAST_STDOUT_FILE}" '
        .success == true
        and .data.rerank.mode == "fusion_only_degraded"
        and .data.rerank.degradedCode == "rerank_model_unavailable"
        and .data.rerank.advisory == {
            "code": "rerank_model_unavailable",
            "severity": "low",
            "permanent": true,
            "message": "No usable local reranker is registered. Search is using fusion-only ranking. Network download is unavailable, but a verified offline reranker artifact can be imported explicitly.",
            "repair": null,
            "resolution": "automatic_repair_unavailable"
        }
        and .data.rerank.rerankScoreCount == 0
        and all(.data.results[]; (has("rerankScore") | not))
    ' "rejected corrupt artifact preserves successful fusion-only fallback"
    replay_order="$(jq -c '[.data.results[].memoryId]' "${LAST_STDOUT_FILE}" 2>/dev/null || printf '[]')"
    assert_eq "${replay_order}" "${initial_order}" \
        "corrupt-artifact fallback preserves deterministic result order"
}

run_model_backed_lane() {
    local require_model="${EE_E2E_NATIVE_RERANK_REQUIRE_MODEL:-0}"
    local degradation_only="${EE_E2E_NATIVE_RERANK_DEGRADATION_ONLY:-0}"
    local archive="${EE_E2E_RERANK_MODEL_ARCHIVE:-}"
    local home_dir="${ROOT}/active-home"
    local workspace="${ROOT}/active-workspace"
    local archive_hash_before archive_hash_after archive_sha256 archive_size
    local unpacked_dir withheld_dir corrupt_order
    local fusion_file reranked_file fusion_order reranked_order fusion_ids reranked_ids

    if [[ -n "${RERANK_VECTOR_OUT}" ]]; then
        require_model=1
    fi
    if [[ "${require_model}" == "1" && "${degradation_only}" == "1" ]]; then
        record_failure "native reranker profile flags do not conflict" \
            "EE_E2E_NATIVE_RERANK_REQUIRE_MODEL=1 cannot be combined with EE_E2E_NATIVE_RERANK_DEGRADATION_ONLY=1"
        return
    fi
    if [[ "${degradation_only}" == "1" ]]; then
        record_skip "model-backed reranker lane" \
            "explicit degradation-only profile; full/default verification runs fail-closed model-backed coverage"
        return
    fi

    if [[ -n "${archive}" ]]; then
        require_model=1
    fi
    if [[ -z "${archive}" ]]; then
        local detail
        detail="EE_E2E_RERANK_MODEL_ARCHIVE is not set"
        if [[ "${require_model}" == "1" ]]; then
            record_failure "explicit model-backed reranker fixture is configured" "${detail}"
        else
            record_skip "model-backed reranker lane" \
                "${detail}; set EE_E2E_NATIVE_RERANK_REQUIRE_MODEL=1 to fail closed"
        fi
        return
    fi
    if [[ ! -r "${archive}" || ! -f "${archive}" ]]; then
        record_failure "explicit model-backed reranker fixture is readable" \
            "missing or unreadable regular file: ${archive}"
        return
    fi

    archive_size="$(wc -c <"${archive}" | tr -d '[:space:]')"
    archive_sha256="$(sha256_file "${archive}")"
    archive_hash_before="$(hash_file "${archive}")"
    assert_eq "${archive_size}" "82767464" \
        "reranker archive byte length matches the pinned manifest"
    assert_eq "${archive_sha256}" \
        "adaada3ccc15ae535e9bea238d2ec05e4f39726bdcad07dd87cba9f85dc10edb" \
        "reranker archive SHA-256 matches the pinned manifest"

    init_workspace "${home_dir}" "${workspace}"
    run_ee_json "doctor_before_reranker_registration" 0 "${home_dir}" "${workspace}" \
        doctor --full --json
    assert_json_file "${LAST_STDOUT_FILE}" '
        .schema == "ee.response.v2"
        and .success == true
        and ([.data.checks[] | select(.name == "reranker_posture")] | length) == 1
        and any(.data.checks[];
            .name == "reranker_posture"
            and .tier == "advisory"
            and .severity == "warning"
            and (.message | startswith("Permanent capability gap:"))
            and (has("repair") | not))
    ' "doctor observes the public pre-registration reranker capability gap"

    run_ee_json "fetch_native_reranker" 0 "${home_dir}" "${workspace}" \
        model fetch rerank-default --from-file "${archive}" --json
    assert_json_file "${LAST_STDOUT_FILE}" '
        .schema == "ee.response.v2"
        and .success == true
        and .data.schema == "ee.model_fetch.v1"
        and .data.modelId == "rerank-default-v1"
        and .data.modelPurpose == "reranker"
        and .data.registryEntry.status == "available"
    ' "verified safetensors reranker is fetched and registered"
    archive_hash_after="$(hash_file "${archive}")"
    assert_eq "${archive_hash_after}" "${archive_hash_before}" \
        "source reranker archive is unchanged by isolated provisioning"

    seed_rerank_corpus "${home_dir}" "${workspace}"
    run_ee_json "hash_embedding_status" 0 "${home_dir}" "${workspace}" index status --json
    assert_json_file "${LAST_STDOUT_FILE}" '
        .success == true
        and .data.embedding.schema == "ee.embedding_posture.v1"
        and .data.embedding.mode == "deterministic_hash"
        and .data.embedding.semantic == false
        and .data.embedding.fast_model_id == "fnv1a-256"
        and .data.embedding.deterministic == true
        and .data.embedding.vector_coverage.embedded >= 5
    ' "native reranker lane deliberately uses deterministic-hash candidates"
    run_ee_json "set_rerank_top_k" 0 "${home_dir}" "${workspace}" \
        config set search.rerank_top_k 5 --json
    run_ee_json "disable_rerank" 0 "${home_dir}" "${workspace}" \
        config set search.rerank off --json
    run_ee_json "fusion_only_search" 0 "${home_dir}" "${workspace}" \
        search "${SEARCH_QUERY}" \
        --limit 5 --relevance-floor 0 --explain --json
    fusion_file="${LAST_STDOUT_FILE}"
    assert_json_file "${fusion_file}" '
        .success == true
        and .data.rerank.mode == "fusion_only"
        and .data.rerank.configured == "off"
        and .data.rerank.rerankScoreCount == 0
        and (.data.results | length) == 5
        and all(.data.results[]; (has("rerankScore") | not))
    ' "explicit rerank-off search is a clean fusion-only baseline"

    run_ee_json "enable_rerank_auto" 0 "${home_dir}" "${workspace}" \
        config set search.rerank auto --json
    run_ee_json "native_reranked_search" 0 "${home_dir}" "${workspace}" \
        search "${SEARCH_QUERY}" \
        --limit 5 --relevance-floor 0 --explain --json
    reranked_file="${LAST_STDOUT_FILE}"
    assert_json_file "${reranked_file}" '
        .success == true
        and .data.rerank.schema == "ee.rerank_posture.v1"
        and .data.rerank.mode == "reranked"
        and .data.rerank.configured == "auto"
        and .data.rerank.available == true
        and .data.rerank.rerankScoreCount == 5
        and (.data.results | length) == 5
        and all(.data.results[];
            .scoreKind == "reranked"
            and (.rerankScore | type) == "number"
            and .rerankScore >= 0 and .rerankScore <= 1
            and .score == .rerankScore)
        and .data.results[0].explanation.factors[0].name == "rerank"
        and any(.data.degraded[]; .code == "embed_model_unavailable")
        and all(.data.degraded[]; .code != "rerank_model_unavailable")
    ' "native reranker is active over hash candidates without rerank degradation"

    fusion_order="$(jq -c '[.data.results[].content]' "${fusion_file}")"
    reranked_order="$(jq -c '[.data.results[].content]' "${reranked_file}")"
    fusion_ids="$(jq -c '[.data.results[].memoryId] | sort' "${fusion_file}")"
    reranked_ids="$(jq -c '[.data.results[].memoryId] | sort' "${reranked_file}")"
    assert_eq "${reranked_ids}" "${fusion_ids}" \
        "reranking preserves the candidate set"
    if [[ "${reranked_order}" != "${fusion_order}" ]]; then
        record_pass "native reranker changes fusion-only ordering"
    else
        record_failure "native reranker changes fusion-only ordering" \
            "reranked order equals fusion-only order"
    fi
    assert_json_file "${reranked_file}" \
        '((.data.results[0].content // "") | startswith("BD1NL1314_RERANK_TARGET"))
        and ((.data.results[1].content // "") | startswith("BD1NL1314_RERANK_TRAP"))
        and (.data.results[0].rerankScore > .data.results[1].rerankScore)' \
        "native reranker ranks the precise release policy above the lexical trap"
    run_ee_json "doctor_after_native_reranked_search" 0 "${home_dir}" "${workspace}" \
        doctor --full --json
    assert_json_file "${LAST_STDOUT_FILE}" '
        .schema == "ee.response.v2"
        and .success == true
        and ([.data.checks[] | select(.name == "reranker_posture")] | length) == 1
        and any(.data.checks[];
            .name == "reranker_posture"
            and .tier == "advisory"
            and .severity == "ok"
            and .message == "Local reranker capability is available; search can apply reranked scoring when configured."
            and (has("repair") | not))
    ' "doctor observes recovery after public registration and real reranked search"
    unpacked_dir="${home_dir}/.local/share/ee/models/rerank/rerank-default-v1/rerank-default-v1"
    withheld_dir="${unpacked_dir}.withheld"
    if [[ "${unpacked_dir}" != "${home_dir}/"* || ! -d "${unpacked_dir}" \
        || -e "${withheld_dir}" ]]; then
        record_failure "isolated registered reranker directory is available to withhold" \
            "expected unique directory beneath isolated HOME: ${unpacked_dir}"
        return
    fi
    if mv "${unpacked_dir}" "${withheld_dir}"; then
        emit_event "note" "$(jq -cn \
            --arg bead "${BEAD_ID}" \
            '{bead_id:$bead,surface:"native_reranker_e2e",label:"registered_model_withheld",artifact_path:"[ISOLATED_HOME]/.local/share/ee/models/rerank/rerank-default-v1/rerank-default-v1.withheld",schema_validation_status:"not_applicable",redaction_status:"passed",sanitized_env:{HOME:"[ISOLATED_HOME]"}}')"
        record_pass "registered reranker removal is isolated and retained"
    else
        record_failure "registered reranker removal is isolated and retained" \
            "failed to withhold isolated unpacked directory: ${unpacked_dir}"
        return
    fi

    run_ee_json "registered_missing_model_search" 0 "${home_dir}" "${workspace}" \
        search "${SEARCH_QUERY}" \
        --limit 5 --relevance-floor 0 --explain --json
    assert_json_file "${LAST_STDOUT_FILE}" '
        .schema == "ee.response.v2"
        and .success == true
        and .data.rerank.mode == "fusion_only_degraded"
        and .data.rerank.available == false
        and .data.rerank.degradedCode == "rerank_model_unavailable"
        and .data.rerank.rerankScoreCount == 0
        and all(.data.results[]; .scoreKind != "reranked" and (has("rerankScore") | not))
        and .data.rerank.advisory.permanent == false
        and .data.rerank.advisory.resolution == "retry_or_inspect_local_registry"
        and any(.data.degraded[]; .code == "rerank_model_unavailable")
    ' "missing registered model degrades explicitly to fusion-only"
    corrupt_order="$(jq -c '[.data.results[].content]' "${LAST_STDOUT_FILE}" 2>/dev/null || printf '[]')"
    assert_eq "${corrupt_order}" "${fusion_order}" \
        "missing registered model restores deterministic fusion-only order"
    write_rerank_vector "${fusion_file}" "${reranked_file}"
}

validate_event_log() {
    jq --exit-status -s '
        def allowed_kind:
            . == "command_start"
            or . == "command_end"
            or . == "assert_ok"
            or . == "assert_fail"
            or . == "golden_compare"
            or . == "timer_lap"
            or . == "note"
            or . == "pack_hash_components"
            or . == "db_generation_observed"
            or . == "volatile_strip"
            or . == "schema_gate"
            or . == "field_selector"
            or . == "bench_iteration"
            or . == "artifact_manifest"
            or . == "lint_determinism"
            or . == "proptest_run"
            or . == "redaction_apply";
        length > 0
        and all(.[];
            type == "object"
            and ((keys - ["schema", "ts", "test_id", "kind", "command", "args", "stdin_hash", "stdout_hash", "stderr_hash", "stderr_excerpt", "exit_code", "elapsed_ms", "fields"]) | length == 0)
            and .schema == "ee.test_event.v1"
            and (.ts | type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
            and (.test_id | type == "string")
            and (.kind | type == "string" and allowed_kind)
            and ((.fields // {}) | type == "object")
            and (if .kind == "assert_ok" then
                    (.fields.label | type == "string")
                elif .kind == "assert_fail" then
                    (.fields.label | type == "string")
                    and (.fields.expected | type == "string")
                    and (.fields.actual | type == "string")
                else true end)
        )
    ' "${EVENT_LOG}" >/dev/null 2>&1
}

finish() {
    local incoming_status=$?
    if [[ "${FINISHED}" == "1" ]]; then
        return "${incoming_status}"
    fi
    FINISHED=1
    trap - EXIT

    if [[ "${incoming_status}" -ne 0 ]]; then
        record_failure "harness reaches finalization without an unexpected exit" \
            "incoming shell status=${incoming_status}"
    fi
    if [[ "${LOGGING_FAILURES}" -gt 0 ]]; then
        record_failure "all structured event writes succeed" \
            "event serialization or append failures=${LOGGING_FAILURES}"
    fi
    if [[ -n "${RERANK_VECTOR_OUT}" ]] \
        && { [[ "${VECTOR_EMITTED}" != "1" ]] \
            || ! validate_rerank_vector "${RERANK_VECTOR_OUT}"; }; then
        record_failure "requested reranker determinism vector is retained" \
            "not emitted by this run, missing, or invalid: ${RERANK_VECTOR_OUT}"
    fi

    if validate_event_log; then
        record_pass "all retained events satisfy the ee.test_event.v1 contract"
    else
        record_failure "all retained events satisfy the ee.test_event.v1 contract" \
            "full event contract validation failed: ${EVENT_LOG}"
    fi

    local verdict="PASS"
    if [[ "${FAILURES}" -gt 0 || "${LOGGING_FAILURES}" -gt 0 ]]; then
        verdict="FAIL"
    fi
    emit_event "note" "$(jq -cn \
        --arg bead "${BEAD_ID}" --arg verdict "${verdict}" \
        --arg logDir "${LOG_DIR}" --arg eventLog "${EVENT_LOG}" \
        --argjson pass "${PASSES}" --argjson fail "${FAILURES}" \
        --argjson skip "${SKIPS}" \
        '{bead_id:$bead,surface:"native_reranker_e2e",label:"final_summary",verdict:$verdict,pass:$pass,fail:$fail,skip:$skip,log_dir:$logDir,event_log:$eventLog,schema_validation_status:"passed_before_final_summary",redaction_status:"passed",sanitized_env:{HOME:"[ISOLATED_HOME]"}}')"

    if ! validate_event_log \
        || ! jq -e -s 'any(.[]; .kind == "note" and .fields.label == "final_summary")' \
            "${EVENT_LOG}" >/dev/null 2>&1; then
        FAILURES=$((FAILURES + 1))
        verdict="FAIL"
        printf '  [FAIL] final event-log validation: %s\n' "${EVENT_LOG}" >&2
    fi
    if [[ "${LOGGING_FAILURES}" -gt 0 ]]; then
        verdict="FAIL"
    fi

    printf 'native_reranker_e2e: verdict=%s pass=%d fail=%d skip=%d\n' \
        "${verdict}" "${PASSES}" "${FAILURES}" "${SKIPS}" >&2
    printf 'Artifacts: %s\n' "${ROOT}"
    local exit_code=0
    if [[ "${FAILURES}" -gt 0 || "${LOGGING_FAILURES}" -gt 0 ]]; then
        exit_code=1
    fi
    exit "${exit_code}"
}
trap finish EXIT

emit_event "note" "$(jq -cn \
    --arg bead "${BEAD_ID}" --arg binaryHash "$(hash_file "${REAL_EE}")" \
    '{bead_id:$bead,surface:"native_reranker_e2e",label:"test_start",ee_binary:"[EE_BINARY]",ee_binary_hash:$binaryHash,artifact_root:"[ARTIFACT_ROOT]",schema_validation_status:"not_applicable",redaction_status:"passed",sanitized_env:{HOME:"[ORIGINAL_HOME]"}}')"

inspect_binary_linkage
run_degradation_lane
run_model_backed_lane
