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

emit_event() {
    local subcommand="$1"
    local attempts="$2"
    local error_class="$3"
    local excerpt="$4"
    local succeeded="$5"
    local recovered_ms="$6"
    local escaped_subcommand escaped_error_class escaped_excerpt
    escaped_subcommand="$(printf '%s' "$subcommand" | json_escape)"
    escaped_error_class="$(printf '%s' "$error_class" | json_escape)"
    escaped_excerpt="$(printf '%s' "$excerpt" | tr '\n' ' ' | cut -c 1-240 | json_escape)"
    printf '{"schema":"ee.beads_retry.v1","subcommand":%s,"attempts":%s,"last_error_class":%s,"last_error_excerpt":%s,"succeeded":%s,"recovered_after_ms":%s,"degraded_codes":["beads_jsonl_partial_write_transient"]}\n' \
        "$escaped_subcommand" "$attempts" "$escaped_error_class" "$escaped_excerpt" "$succeeded" "$recovered_ms" >&2
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

subcommand="$1"
backoffs_ms=(0 50 200 500)
attempt=0
last_error_class=""
last_stderr=""
start_ms="$(python3 -c 'import time; print(int(time.monotonic() * 1000))')"

while [ "$attempt" -lt "${#backoffs_ms[@]}" ]; do
    if [ "${backoffs_ms[$attempt]}" -gt 0 ]; then
        sleep "$(python3 - "${backoffs_ms[$attempt]}" <<'PY'
import sys
print(f"{int(sys.argv[1]) / 1000:.3f}")
PY
)"
    fi

    stdout_file="$(mktemp "${TMPDIR:-/tmp}/br-retry-stdout.XXXXXX")" || exit 2
    stderr_file="$(mktemp "${TMPDIR:-/tmp}/br-retry-stderr.XXXXXX")" || exit 2

    br "$@" >"$stdout_file" 2>"$stderr_file"
    status=$?
    stderr_text="$(cat "$stderr_file")"

    if [ "$status" -eq 0 ]; then
        cat "$stdout_file"
        if [ "$attempt" -gt 0 ]; then
            now_ms="$(python3 -c 'import time; print(int(time.monotonic() * 1000))')"
            emit_event "$subcommand" "$((attempt + 1))" "$last_error_class" "$last_stderr" "true" "$((now_ms - start_ms))"
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
        emit_event "$subcommand" 1 "$last_error_class" "$stderr_text" "false" 0
    fi

    attempt=$((attempt + 1))
done

printf '%s' "$last_stderr" >&2
now_ms="$(python3 -c 'import time; print(int(time.monotonic() * 1000))')"
emit_event "$subcommand" "$attempt" "$last_error_class" "$last_stderr" "false" "$((now_ms - start_ms))"
exit 1
