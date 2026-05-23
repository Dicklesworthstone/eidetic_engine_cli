#!/usr/bin/env bash
# Logged e2e driver for bd-t2bgr.5.
#
# Exercises the public `ee import cass` path against a deterministic fake CASS
# binary. The fake binary is configured through EE_CASS_BINARY so the import
# surface uses the same absolute-path trust path as production imports.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
if ! command -v python3 >/dev/null 2>&1; then
    echo "cass_supervision: python3 is required" >&2
    exit 2
fi

if [ -z "${EE_E2E_TMPDIR:-}" ]; then
    case "${TMPDIR:-}" in
        /Volumes/*)
            export EE_E2E_TMPDIR="/tmp/ee-cass-subprocess-supervision-e2e"
            mkdir -p "$EE_E2E_TMPDIR"
            ;;
    esac
fi

epic_setup "cass_subprocess_supervision"

FAKE_CASS_DIR="$EPIC_WORKSPACE/fake-cass/bin"
FAKE_CASS_BIN="$FAKE_CASS_DIR/cass"
FAKE_SESSION_PATH="$EPIC_WORKSPACE/fake-cass/session.jsonl"

mkdir -p "$FAKE_CASS_DIR"
chmod 755 "$FAKE_CASS_DIR"
printf '{"schema":"fake.cass.session.v1"}\n' >"$FAKE_SESSION_PATH"

cat >"$FAKE_CASS_BIN" <<'FAKE_CASS'
#!/usr/bin/env bash
set -euo pipefail

scenario="${EE_FAKE_CASS_SCENARIO:-success}"
marker="${EE_FAKE_CASS_MARKER:-}"
session_path="${EE_FAKE_CASS_SESSION_PATH:?EE_FAKE_CASS_SESSION_PATH required}"

case "${1:-}" in
    sessions)
        shift
        workspace=""
        while [ "$#" -gt 0 ]; do
            case "${1:-}" in
                --workspace)
                    workspace="${2:-}"
                    shift 2
                    ;;
                --json|--limit)
                    if [ "${1:-}" = "--limit" ]; then
                        shift 2
                    else
                        shift
                    fi
                    ;;
                --data-dir)
                    shift 2
                    ;;
                *)
                    shift
                    ;;
            esac
        done
        python3 - "$workspace" "$session_path" <<'PY'
import json
import sys

workspace, session_path = sys.argv[1:]
json.dump(
    {
        "sessions": [
            {
                "path": session_path,
                "workspace": workspace,
                "agent": "codex",
                "started_at": "2026-05-22T00:00:00Z",
                "modified": "2026-05-22T00:00:01Z",
                "message_count": 2,
                "token_count": 16,
            }
        ]
    },
    sys.stdout,
    separators=(",", ":"),
)
sys.stdout.write("\n")
PY
        ;;
    view)
        case "$scenario" in
            success)
                python3 - <<'PY'
import json
import sys

rows = [
    {
        "line": 1,
        "content": json.dumps(
            {"type": "message", "message": {"role": "user", "content": "remember subprocess supervision"}}
        ),
    },
    {
        "line": 2,
        "content": json.dumps(
            {"type": "message", "message": {"role": "assistant", "content": "captured fake CASS span"}}
        ),
    },
]
for row in rows:
    sys.stdout.write(json.dumps(row, separators=(",", ":")) + "\n")
PY
                ;;
            stdout_cap)
                python3 - <<'PY'
import sys

sys.stdout.write("x" * (1024 * 1024 + 3))
sys.stdout.write("\n")
sys.stdout.flush()
PY
                sleep 1
                if [ -n "$marker" ]; then
                    printf 'survived\n' >"$marker"
                fi
                ;;
            timeout)
                sleep 35
                if [ -n "$marker" ]; then
                    printf 'survived\n' >"$marker"
                fi
                ;;
            *)
                printf 'unknown fake CASS scenario: %s\n' "$scenario" >&2
                exit 2
                ;;
        esac
        ;;
    *)
        printf 'unexpected fake CASS command: %s\n' "${1:-<empty>}" >&2
        exit 2
        ;;
esac
FAKE_CASS
chmod 755 "$FAKE_CASS_BIN"

response_file_for() {
    local stdout_file="${1:?stdout file required}"
    local stderr_file="${2:?stderr file required}"
    if jq -e . "$stdout_file" >/dev/null 2>&1; then
        printf '%s\n' "$stdout_file"
    elif jq -e . "$stderr_file" >/dev/null 2>&1; then
        printf '%s\n' "$stderr_file"
    else
        printf '\n'
    fi
}

emit_cass_case_event() {
    local scenario="${1:?scenario required}"
    local phase="${2:?phase required}"
    local status="${3:?status required}"
    local exit_code="${4:?exit code required}"
    local elapsed_ms="${5:?elapsed ms required}"
    local stdout_file="${6:?stdout file required}"
    local stderr_file="${7:?stderr file required}"
    local response_file="${8:-}"
    local marker_path="${9:-}"
    local response_schema="" data_schema="" error_code="" first_failure="" marker_exists=false

    if [ -n "$response_file" ]; then
        response_schema="$(jq -r '.schema // empty' "$response_file" 2>/dev/null || true)"
        data_schema="$(jq -r '.data.schema // empty' "$response_file" 2>/dev/null || true)"
        error_code="$(jq -r '.error.code // empty' "$response_file" 2>/dev/null || true)"
        first_failure="$(jq -r '.error.message // empty' "$response_file" 2>/dev/null || true)"
    fi
    if [ -z "$first_failure" ] && [ -s "$stderr_file" ]; then
        first_failure="$(head -c 512 "$stderr_file")"
    fi
    if [ -n "$marker_path" ] && [ -e "$marker_path" ]; then
        marker_exists=true
    fi

    _e2e_emit_event "cass_supervision" \
        "scenario" "$scenario" \
        "phase" "$phase" \
        "status" "$status" \
        "command" "ee import cass --workspace <workspace> --database <case-db> --limit 1 --json" \
        "workspace" "$EPIC_WORKSPACE" \
        "fake_cass_binary" "$FAKE_CASS_BIN" \
        "fake_cass_scenario" "$scenario" \
        "stdout_artifact" "$stdout_file" \
        "stderr_artifact" "$stderr_file" \
        "response_file" "$response_file" \
        "response_schema" "$response_schema" \
        "data_schema" "$data_schema" \
        "error_code" "$error_code" \
        "marker_path" "$marker_path" \
        "marker_exists" "$marker_exists" \
        "first_failure" "$first_failure" \
        "exit_code" "$exit_code" \
        "elapsed_ms" "$elapsed_ms"
}

run_cass_case() {
    local scenario="${1:?scenario required}"
    local expected_rc="${2:?expected rc required}"
    local max_elapsed_ms="${3:?max elapsed ms required}"
    local case_dir="$EPIC_WORKSPACE/cases/$scenario"
    local stdout_file="$case_dir/stdout.json"
    local stderr_file="$case_dir/stderr.txt"
    local database_file="$case_dir/ee.db"
    local marker_file="$case_dir/child-survived.marker"
    local started ended elapsed_ms rc response_file status

    mkdir -p "$case_dir"
    export EE_CASS_BINARY="$FAKE_CASS_BIN"
    export EE_FAKE_CASS_SCENARIO="$scenario"
    export EE_FAKE_CASS_SESSION_PATH="$FAKE_SESSION_PATH"
    export EE_FAKE_CASS_MARKER="$marker_file"

    emit_cass_case_event "$scenario" "start" "running" 0 0 "$stdout_file" "$stderr_file" "" "$marker_file"
    started="$(python3 -c 'import time; print(time.monotonic_ns())')"
    "$EE_BINARY" import cass \
        --workspace "$EPIC_WORKSPACE" \
        --database "$database_file" \
        --limit 1 \
        --json >"$stdout_file" 2>"$stderr_file"
    rc=$?
    ended="$(python3 -c 'import time; print(time.monotonic_ns())')"
    elapsed_ms="$(python3 -c "print(($ended - $started) / 1_000_000.0)")"
    response_file="$(response_file_for "$stdout_file" "$stderr_file")"

    status="passed"
    if [ "$expected_rc" = "0" ]; then
        if [ "$rc" -ne 0 ]; then
            status="failed"
        fi
    elif [ "$rc" -eq 0 ]; then
        status="failed"
    fi

    emit_cass_case_event "$scenario" "end" "$status" "$rc" "$elapsed_ms" "$stdout_file" "$stderr_file" "$response_file" "$marker_file"

    if [ "$expected_rc" = "0" ]; then
        e2e_log_assert_eq "$rc" "0" "${scenario}_exit_code"
        assert_jq "$(cat "$response_file")" '.success' "true" "${scenario}_success_envelope"
        assert_jq "$(cat "$response_file")" '.data.schema' "ee.import.cass.v1" "${scenario}_data_schema"
        assert_jq "$(cat "$response_file")" '.data.sessionsImported' "1" "${scenario}_sessions_imported"
        assert_jq "$(cat "$response_file")" '.data.spansImported' "2" "${scenario}_spans_imported"
    else
        e2e_log_assert_num "$rc" -ne 0 "${scenario}_nonzero_exit"
        if [ -n "$response_file" ]; then
            assert_jq "$(cat "$response_file")" '.schema' "ee.error.v2" "${scenario}_error_envelope"
        else
            EE_TEST_LOG_ASSERTS_FAIL=$((EE_TEST_LOG_ASSERTS_FAIL + 1))
            _e2e_emit_event "assert_fail" "label" "${scenario}_error_envelope" "expected" "json error" "actual" "missing"
        fi
        e2e_log_assert_eq "$(test -e "$marker_file" && printf yes || printf no)" "no" "${scenario}_child_not_survived"
    fi

    python3 - "$elapsed_ms" "$max_elapsed_ms" "$scenario" <<'PY'
import sys

elapsed = float(sys.argv[1])
limit = float(sys.argv[2])
scenario = sys.argv[3]
if elapsed > limit:
    print(f"{scenario} exceeded elapsed limit: {elapsed} > {limit}", file=sys.stderr)
    sys.exit(1)
PY
    e2e_log_assert_num "${elapsed_ms%.*}" -le "$max_elapsed_ms" "${scenario}_elapsed_bound"
}

run_cass_case "success" "0" "5000"
run_cass_case "stdout_cap" "nonzero" "5000"
run_cass_case "timeout" "nonzero" "45000"

e2e_log_note "cass_subprocess_supervision complete log=${EE_TEST_LOG_PATH:-}"
if [ "$EE_TEST_LOG_ASSERTS_FAIL" -ne 0 ]; then
    exit 3
fi
