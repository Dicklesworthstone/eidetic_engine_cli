#!/bin/sh
# bd-1h8ji.4 — RCH remote-portability transcript diagnostic.
#
# Reads a remote-verifier transcript (from stdin OR a file path arg) and
# flags Mac-only artifacts that should never appear when the command was
# supposed to run on a remote Linux worker:
#
#   - Mac-only Rust target triples: `*-apple-darwin*`, `*-apple-macosx*`,
#     `arm64-apple-*`, `x86_64-apple-*`.
#   - Mac-local USB scratch paths: `/Volumes/USBNVME16TB`, `/Volumes/...`.
#   - Mac-only TMPDIR fingerprints: `/var/folders/`.
#   - AppleDouble metadata files (`._*`, `.DS_Store`) flowing into compile
#     transcripts — vendored C crates such as `zstd-sys` will attempt to
#     compile `._foo.c` siblings if they reach the worker file set.
#
# When invoked with `--json`, writes an `ee.rch_remote_portability.v1`
# report to stdout (and to the path in `EE_RCH_PORTABILITY_REPORT` when
# set) and exits non-zero if any portability anomaly was found. Without
# `--json`, prints a human-readable summary.
#
# Usage:
#   scripts/check-rch-portability.sh [--json] [<transcript-path>]
#   <command-producing-transcript> | scripts/check-rch-portability.sh --json
#
# Exit codes: 0 = clean, 1 = portability anomaly found.
#
# This is a static diagnostic; it does NOT mutate the transcript, files,
# or any verification ledger. The full TMPDIR/CARGO_TARGET_DIR sanitizer
# wrapper is the in-flight follow-up slice; this script is the read-only
# detection half of `bd-1h8ji.4`.

set -eu

REPORT_SCHEMA="ee.rch_remote_portability.v1"
JSON_OUTPUT=false
TRANSCRIPT_PATH=""
SELF_TEST=false

usage() {
    sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --json) JSON_OUTPUT=true; shift ;;
        --self-test) SELF_TEST=true; shift ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        -*) printf 'unknown flag: %s\n' "$1" >&2; usage >&2; exit 2 ;;
        *) TRANSCRIPT_PATH="$1"; shift ;;
    esac
done

read_transcript() {
    if [ -n "$TRANSCRIPT_PATH" ]; then
        if [ ! -r "$TRANSCRIPT_PATH" ]; then
            printf 'transcript not readable: %s\n' "$TRANSCRIPT_PATH" >&2
            exit 2
        fi
        cat "$TRANSCRIPT_PATH"
    else
        cat
    fi
}

# Each pattern is a fingerprint => anomaly code => human description.
# Keeping the table here (rather than in a separate file) so the script
# is self-contained and the contract is auditable in one place.
PATTERNS_CODES_DESCRIPTIONS=$(
    cat <<'PATTERNS'
darwin_target_triple|rch_portability_darwin_target|Rust target triple references Apple/Darwin during supposed remote Linux verification
apple_target_triple|rch_portability_darwin_target|Apple target triple slipped through (apple-darwin or apple-macosx)
volumes_usb_path|rch_portability_usb_volume|Mac-local /Volumes/... USB scratch path appeared in a remote command transcript
var_folders_tmp|rch_portability_var_folders_tmp|Mac /var/folders/ TMPDIR leaked into a remote command
appledouble_compile|rch_portability_appledouble_compile|AppleDouble (._*) metadata file entered the remote compile file set
ds_store_file|rch_portability_ds_store|.DS_Store metadata file appeared in the remote transfer
PATTERNS
)

# Anomaly detection runs as a single pass over the transcript text.
# Each line in the output table is "<code>\t<description>\t<example>".
detect_anomalies() {
    local text="$1"
    local code
    local desc
    local example

    while IFS='|' read -r kind code desc; do
        [ -n "$code" ] || continue
        example=""
        case "$kind" in
            darwin_target_triple)
                example=$(printf '%s' "$text" | grep -Eo '([A-Za-z0-9_]+-)?apple-darwin[A-Za-z0-9._-]*' | head -n 1 || true)
                ;;
            apple_target_triple)
                example=$(printf '%s' "$text" | grep -Eo '([A-Za-z0-9_]+-)?apple-(macosx|ios)[A-Za-z0-9._-]*' | head -n 1 || true)
                ;;
            volumes_usb_path)
                example=$(printf '%s' "$text" | grep -Eo '/Volumes/[A-Za-z0-9_./-]+' | head -n 1 || true)
                ;;
            var_folders_tmp)
                example=$(printf '%s' "$text" | grep -Eo '/var/folders/[A-Za-z0-9_./+=-]+' | head -n 1 || true)
                ;;
            appledouble_compile)
                example=$(printf '%s' "$text" | grep -Eo '(^|[[:space:]/"])\._[A-Za-z0-9._-]+' | head -n 1 || true)
                example=$(printf '%s' "$example" | sed -E 's/^[[:space:]"]*//; s/^\///')
                ;;
            ds_store_file)
                example=$(printf '%s' "$text" | grep -Eo '\.DS_Store' | head -n 1 || true)
                ;;
        esac
        if [ -n "$example" ]; then
            printf '%s\t%s\t%s\n' "$code" "$desc" "$example"
        fi
    done <<EOF
$PATTERNS_CODES_DESCRIPTIONS
EOF
}

emit_human_report() {
    local count="$1"
    local body="$2"
    if [ "$count" -eq 0 ]; then
        printf '[rch portability] clean: no Mac-local artifacts found in transcript.\n'
        return 0
    fi
    printf '[rch portability] %d anomaly(ies) found:\n' "$count"
    printf '%s' "$body" | while IFS=$(printf '\t') read -r code desc example; do
        [ -n "$code" ] || continue
        printf '  - [%s] %s\n      example: %s\n' "$code" "$desc" "$example"
    done
}

emit_json_report() {
    local count="$1"
    local body="$2"
    local anomalies_json="[]"
    if [ -n "$body" ] && command -v jq >/dev/null 2>&1; then
        anomalies_json=$(printf '%s' "$body" |
            awk -F$'\t' 'NF>=3 {
                gsub(/"/, "\\\"", $1); gsub(/"/, "\\\"", $2); gsub(/"/, "\\\"", $3)
                printf "{\"code\":\"%s\",\"description\":\"%s\",\"example\":\"%s\"}\n", $1, $2, $3
            }' |
            jq -s '.')
    fi
    local status="ok"
    if [ "$count" -gt 0 ]; then status="anomalies_found"; fi
    if command -v jq >/dev/null 2>&1; then
        jq -cn \
            --arg schema "$REPORT_SCHEMA" \
            --arg status "$status" \
            --argjson count "$count" \
            --argjson anomalies "$anomalies_json" \
            '{schema:$schema,status:$status,count:$count,anomalies:$anomalies}'
    else
        printf '{"schema":"%s","status":"%s","count":%d,"anomalies":[]}\n' \
            "$REPORT_SCHEMA" "$status" "$count"
    fi
}

run_self_test() {
    local fixture
    fixture="$(cat <<'EOF'
remote command: cargo build --target x86_64-unknown-linux-gnu --target arm64-apple-darwin22.0
syncing /Volumes/USBNVME16TB/temp_agent_space/cargo-target -> /data/projects/...
warning: vendor/zstd-sys/c/._zstd.c contains AppleDouble metadata
TMPDIR=/var/folders/abc/xyz123/T/cargo-build-xxxx
EOF
)"
    local body
    body=$(detect_anomalies "$fixture")
    local count
    count=$(printf '%s' "$body" | grep -c .)
    [ "$count" -ge 4 ] || {
        printf 'self-test FAILED: expected >=4 anomalies, got %d\n' "$count" >&2
        printf '%s\n' "$body" >&2
        exit 1
    }
    printf 'self-test PASSED: detected %d anomalies in fixture\n' "$count"
    exit 0
}

if [ "$SELF_TEST" = true ]; then
    run_self_test
fi

TRANSCRIPT=$(read_transcript)
BODY=$(detect_anomalies "$TRANSCRIPT" || true)
if [ -n "$BODY" ]; then
    COUNT=$(printf '%s' "$BODY" | grep -c . || true)
else
    COUNT=0
fi

if [ "$JSON_OUTPUT" = true ]; then
    REPORT=$(emit_json_report "$COUNT" "$BODY")
    if [ -n "${EE_RCH_PORTABILITY_REPORT:-}" ]; then
        printf '%s\n' "$REPORT" > "$EE_RCH_PORTABILITY_REPORT"
    fi
    printf '%s\n' "$REPORT"
else
    emit_human_report "$COUNT" "$BODY"
fi

if [ "$COUNT" -gt 0 ]; then
    exit 1
fi
exit 0
