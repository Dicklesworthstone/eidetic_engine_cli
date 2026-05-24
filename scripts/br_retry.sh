#!/usr/bin/env bash
# Retry transient br JSONL partial-read failures caused by concurrent
# br sync --flush-only rewrites.

set -uo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/br_retry.sh <br-subcommand> [args...]

Examples:
  scripts/br_retry.sh ready --json
  scripts/br_retry.sh list --status open --json
  scripts/br_retry.sh stats --json

Environment:
  BR_RETRY_ATTEMPT_TIMEOUT_MS  Per-attempt br timeout budget (default: 10000)
  BR_RETRY_TAIL_BYTES          Diagnostic stdout/stderr tail size (default: 1024)
  BR_RETRY_TMPDIR              Retained diagnostic artifact directory (default: /tmp)

Retries only transient Beads JSONL partial-write parse signatures:
  - CONFIG_ERROR / Invalid JSON at line N
  - invalid type: ..., expected struct Issue

All stdout from the successful br invocation is passed through unchanged.
Diagnostics are emitted to stderr as ee.beads_retry.v1 JSON lines.
EOF
}

json_escape() {
    python3 -c 'import json, sys; print(json.dumps(sys.stdin.read()))'
}

now_ms() {
    python3 -c 'import time; print(int(time.monotonic() * 1000))'
}

json_field() {
    local field="$1"
    python3 -c 'import json, sys; print(json.load(sys.stdin)[sys.argv[1]])' "$field"
}

positive_integer_or_die() {
    local name="$1"
    local value="$2"
    case "$value" in
        ''|*[!0-9]*)
            echo "br_retry: $name must be a positive integer, got: $value" >&2
            exit 2
            ;;
        0)
            echo "br_retry: $name must be greater than zero" >&2
            exit 2
            ;;
    esac
}

file_bytes() {
    local path="$1"
    wc -c <"$path" | tr -d ' '
}

file_tail() {
    local path="$1"
    if [ ! -s "$path" ]; then
        return 0
    fi
    tail -c "$BR_RETRY_TAIL_BYTES" "$path"
}

run_br_once() {
    local stdout_file="$1"
    local stderr_file="$2"
    shift 2
    python3 - "$BR_RETRY_ATTEMPT_TIMEOUT_MS" "$stdout_file" "$stderr_file" "$@" <<'PY'
import json
import subprocess
import sys
import time

timeout_ms = int(sys.argv[1])
stdout_path = sys.argv[2]
stderr_path = sys.argv[3]
argv = sys.argv[4:]
started = time.monotonic()
timed_out = False
status = 0

with open(stdout_path, "wb") as stdout_file, open(stderr_path, "wb") as stderr_file:
    process = subprocess.Popen(argv, stdout=stdout_file, stderr=stderr_file)
    try:
        status = process.wait(timeout=timeout_ms / 1000)
    except subprocess.TimeoutExpired:
        timed_out = True
        process.terminate()
        try:
            process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            # Only kill the child spawned by this wrapper; never scan for or kill peers.
            process.kill()
            process.wait()
        status = 124

elapsed_ms = int((time.monotonic() - started) * 1000)
print(json.dumps({"status": status, "timed_out": timed_out, "elapsed_ms": elapsed_ms}, separators=(",", ":")))
PY
}

emit_event() {
    local subcommand="$1"
    local attempts="$2"
    local error_class="$3"
    local excerpt="$4"
    local succeeded="$5"
    local recovered_ms="$6"
    local attempt_elapsed_ms="$7"
    local timed_out="$8"
    local degraded_code="$9"
    local stdout_file="${10}"
    local stderr_file="${11}"
    local escaped_subcommand escaped_error_class escaped_excerpt escaped_stdout_tail escaped_stderr_tail escaped_stdout_path escaped_stderr_path
    local stdout_bytes stderr_bytes
    escaped_subcommand="$(printf '%s' "$subcommand" | json_escape)"
    escaped_error_class="$(printf '%s' "$error_class" | json_escape)"
    escaped_excerpt="$(printf '%s' "$excerpt" | tr '\n' ' ' | cut -c 1-240 | json_escape)"
    stdout_bytes="$(file_bytes "$stdout_file")"
    stderr_bytes="$(file_bytes "$stderr_file")"
    escaped_stdout_tail="$(file_tail "$stdout_file" | json_escape)"
    escaped_stderr_tail="$(file_tail "$stderr_file" | json_escape)"
    escaped_stdout_path="$(printf '%s' "$stdout_file" | json_escape)"
    escaped_stderr_path="$(printf '%s' "$stderr_file" | json_escape)"
    printf '{"schema":"ee.beads_retry.v1","subcommand":%s,"attempts":%s,"last_error_class":%s,"last_error_excerpt":%s,"succeeded":%s,"recovered_after_ms":%s,"attempt_timeout_ms":%s,"attempt_elapsed_ms":%s,"timed_out":%s,"stdout_bytes":%s,"stderr_bytes":%s,"stdout_tail":%s,"stderr_tail":%s,"artifacts":[{"kind":"stdout","path":%s},{"kind":"stderr","path":%s}],"recovery":[{"priority":1,"kind":"retry_or_inspect_retained_artifacts","command":null,"message":"Inspect retained stdout/stderr artifacts before retrying if the source stays degraded."}],"degraded_codes":["%s"]}\n' \
        "$escaped_subcommand" "$attempts" "$escaped_error_class" "$escaped_excerpt" "$succeeded" "$recovered_ms" \
        "$BR_RETRY_ATTEMPT_TIMEOUT_MS" "$attempt_elapsed_ms" "$timed_out" "$stdout_bytes" "$stderr_bytes" \
        "$escaped_stdout_tail" "$escaped_stderr_tail" "$escaped_stdout_path" "$escaped_stderr_path" "$degraded_code" >&2
}

classify_error() {
    local stderr_text="$1"
    if printf '%s' "$stderr_text" | grep -Eiq 'CONFIG_ERROR|Configuration error'; then
        if printf '%s' "$stderr_text" | grep -Eiq 'Invalid JSON at line [0-9]+'; then
            printf '%s\n' "invalid_json_line"
            return 0
        fi
    fi
    if printf '%s' "$stderr_text" | grep -Eiq 'invalid type:.*expected struct Issue'; then
        printf '%s\n' "invalid_issue_record"
        return 0
    fi
    return 1
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    usage
    exit 0
fi

if [ "$#" -lt 1 ]; then
    usage >&2
    exit 2
fi

if ! command -v br >/dev/null 2>&1; then
    echo "br_retry: br not found in PATH" >&2
    exit 127
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "br_retry: python3 is required for diagnostic JSON escaping" >&2
    exit 127
fi

BR_RETRY_ATTEMPT_TIMEOUT_MS="${BR_RETRY_ATTEMPT_TIMEOUT_MS:-10000}"
BR_RETRY_TAIL_BYTES="${BR_RETRY_TAIL_BYTES:-1024}"
BR_RETRY_TMPDIR="${BR_RETRY_TMPDIR:-/tmp}"
positive_integer_or_die "BR_RETRY_ATTEMPT_TIMEOUT_MS" "$BR_RETRY_ATTEMPT_TIMEOUT_MS"
positive_integer_or_die "BR_RETRY_TAIL_BYTES" "$BR_RETRY_TAIL_BYTES"

subcommand="$1"
backoffs_ms=(0 50 200 500)
attempt=0
last_error_class=""
last_stderr=""
start_ms="$(now_ms)"

while [ "$attempt" -lt "${#backoffs_ms[@]}" ]; do
    if [ "${backoffs_ms[$attempt]}" -gt 0 ]; then
        sleep "$(python3 - "${backoffs_ms[$attempt]}" <<'PY'
import sys
print(f"{int(sys.argv[1]) / 1000:.3f}")
PY
)"
    fi

    stdout_file="$(mktemp "$BR_RETRY_TMPDIR/br-retry-stdout.XXXXXX")" || exit 2
    stderr_file="$(mktemp "$BR_RETRY_TMPDIR/br-retry-stderr.XXXXXX")" || exit 2

    run_result="$(run_br_once "$stdout_file" "$stderr_file" br "$@")"
    status="$(printf '%s' "$run_result" | json_field status)"
    timed_out="$(printf '%s' "$run_result" | json_field timed_out)"
    attempt_elapsed_ms="$(printf '%s' "$run_result" | json_field elapsed_ms)"
    stderr_text="$(cat "$stderr_file")"

    if [ "$timed_out" = "True" ] || [ "$timed_out" = "true" ]; then
        last_stderr="$stderr_text"
        emit_event "$subcommand" "$((attempt + 1))" "command_timeout" "$stderr_text" "false" "$attempt_elapsed_ms" "$attempt_elapsed_ms" "true" "beads_command_timeout" "$stdout_file" "$stderr_file"
        exit 124
    fi

    if [ "$status" -eq 0 ]; then
        cat "$stdout_file"
        if [ "$attempt" -gt 0 ]; then
            end_ms="$(now_ms)"
            emit_event "$subcommand" "$((attempt + 1))" "$last_error_class" "$last_stderr" "true" "$((end_ms - start_ms))" "$attempt_elapsed_ms" "false" "beads_jsonl_partial_write_transient" "$stdout_file" "$stderr_file"
        fi
        exit 0
    fi

    last_stderr="$stderr_text"
    if ! last_error_class="$(classify_error "$stderr_text")"; then
        cat "$stdout_file"
        cat "$stderr_file" >&2
        exit "$status"
    fi

    if [ "$attempt" -eq 0 ]; then
        emit_event "$subcommand" 1 "$last_error_class" "$stderr_text" "false" 0 "$attempt_elapsed_ms" "false" "beads_jsonl_partial_write_transient" "$stdout_file" "$stderr_file"
    fi

    attempt=$((attempt + 1))
done

printf '%s' "$last_stderr" >&2
end_ms="$(now_ms)"
emit_event "$subcommand" "$attempt" "$last_error_class" "$last_stderr" "false" "$((end_ms - start_ms))" "0" "false" "beads_jsonl_partial_write_transient" "$stdout_file" "$stderr_file"
exit 1
