#!/usr/bin/env bash
# bd-2caru.5 — read-pool concurrency and perf-evidence e2e driver.
#
# This script drives real `ee` child processes against one temporary workspace:
#   - seeds a deterministic corpus large enough for non-trivial context reads
#   - runs concurrent `ee context` readers while `ee remember` writers commit
#   - verifies every context response is valid JSON with a pack hash
#   - verifies writer results are successful or structured contention responses
#   - records p50 latency for pool_size=1 and pool_size=8 batches
#
# This is a process-fanout CLI harness: each `ee context` is a separate process.
# The read pool is process-local, so this script can prove no-deadlock,
# structured contention, pack hashes, and logging, but it cannot prove an
# in-process pool speedup. Dedicated perf runs must use an in-process fanout
# harness or benchmark before enforcing the pool_size=8 <= 60% of pool_size=1
# acceptance gate.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
if ! command -v python3 >/dev/null 2>&1; then
    echo "read-pool: python3 is required" >&2
    exit 2
fi

epic_setup "read_pool_concurrency"

RUN_SUFFIX="$(printf '%s' "$$" | tr '0123456789' 'abcdefghij')"
RUN_ID="readpool${RUN_SUFFIX}"
ARTIFACT_DIR="$EPIC_WORKSPACE/read-pool-artifacts"
mkdir -p "$ARTIFACT_DIR"

READ_POOL_CONTEXTS="${EE_READ_POOL_CONTEXTS:-8}"
READ_POOL_WRITERS="${EE_READ_POOL_WRITERS:-4}"
READ_POOL_CORPUS_SIZE="${EE_READ_POOL_CORPUS_SIZE:-220}"
READ_POOL_ENFORCE_SPEEDUP="${EE_READ_POOL_ENFORCE_SPEEDUP:-0}"
READ_POOL_HARNESS_MODE="process_fanout_cli"

read_pool_step() {
    local phase="${1:?phase required}"
    local valid="${2:?valid required}"
    local detail="${3:?detail required}"
    local elapsed_ms="${4:-0}"
    local pool_size="${5:-}"
    local artifact_hash="${6:-}"
    _e2e_emit_event "read_pool_concurrency_e2e" \
        "schema" "ee.test_event.v1" \
        "kind" "read_pool_concurrency_e2e" \
        "phase" "$phase" \
        "valid" "$valid" \
        "detail" "$detail" \
        "workspace_id" "$RUN_ID" \
        "request_id" "$RUN_ID-$phase" \
        "bead_id" "bd-2caru.5" \
        "surface" "read_pool" \
        "harness_mode" "$READ_POOL_HARNESS_MODE" \
        "elapsed_ms" "$elapsed_ms" \
        "pool_size" "$pool_size" \
        "artifactHash" "$artifact_hash"
}

write_read_pool_config() {
    local pool_size="${1:?pool size required}"
    cat >"$EPIC_WORKSPACE/.ee/config.toml" <<EOF
[storage.read_pool]
size = $pool_size
idle_timeout_seconds = 30
pin_snapshot = true
EOF
    read_pool_step "config" "true" "pool_size=$pool_size" "0" "$pool_size"
}

seed_read_pool_corpus() {
    local import_dir import_file i memory_id
    import_dir="$EPIC_WORKSPACE/read-pool-import"
    import_file="$import_dir/memories.jsonl"
    mkdir -p "$import_dir"
    printf '{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-05-16T00:00:00Z","workspace_id":"ws_%s","workspace_path":"/read-pool/%s","export_scope":"memories","redaction_level":"standard","record_count":%s,"ee_version":"bd-2caru.5","hostname":null,"export_id":"exp_%s","import_source":"native","trust_level":"validated","checksum":null,"signature":null,"source_schema_version":null}\n' \
        "$RUN_ID" "$RUN_ID" "$READ_POOL_CORPUS_SIZE" "$RUN_ID" >"$import_file"
    for i in $(seq 1 "$READ_POOL_CORPUS_SIZE"); do
        memory_id="$(printf 'mem_%026d' "$i")"
        printf '{"schema":"ee.export.memory.v1","memory_id":"%s","workspace_id":"ws_%s","level":"procedural","kind":"rule","content":"Read pool concurrency fixture %s for %s: context readers must see stable packs while writers commit.","importance":0.8,"confidence":0.8,"utility":0.8,"created_at":"2026-05-16T00:00:01Z","updated_at":null,"tombstoned_at":null,"tombstoned_reason":null,"valid_from":null,"valid_to":null,"expires_at":null,"source_agent":"bd-2caru.5","provenance_uri":"ee-export://bd-2caru.5/%s","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}\n' \
            "$memory_id" "$RUN_ID" "$i" "$RUN_ID" "$i" >>"$import_file"
    done

    if ee_workspace import jsonl --source "$import_file" --json >/dev/null 2>&1; then
        read_pool_step "setup" "true" "seeded=$READ_POOL_CORPUS_SIZE" "0" "" "$(_e2e_hash_file "$import_file")"
    else
        read_pool_step "setup" "false" "import_failed" "0" "" "$(_e2e_hash_file "$import_file")"
        exit 3
    fi
}

spawn_context_reader() {
    local label="${1:?label required}"
    local pool_size="${2:?pool size required}"
    local out_file="$ARTIFACT_DIR/$label.out"
    local err_file="$ARTIFACT_DIR/$label.err"
    local meta_file="$ARTIFACT_DIR/$label.meta"
    (
        started="$(python3 -c 'import time; print(time.monotonic_ns())')"
        set +e
        "$EE_BINARY" context \
            "read pool concurrency fixture $RUN_ID stable pack" \
            --workspace "$EPIC_WORKSPACE" \
            --candidate-pool 80 \
            --max-tokens 2000 \
            --json >"$out_file" 2>"$err_file"
        rc=$?
        set -e
        ended="$(python3 -c 'import time; print(time.monotonic_ns())')"
        elapsed_ms="$(python3 -c "print(int(($ended - $started) / 1_000_000))")"
        printf 'rc=%s\nelapsed_ms=%s\npool_size=%s\n' "$rc" "$elapsed_ms" "$pool_size" >"$meta_file"
    ) &
    SPAWNED_PID="$!"
}

spawn_writer() {
    local label="${1:?label required}"
    local out_file="$ARTIFACT_DIR/$label.out"
    local err_file="$ARTIFACT_DIR/$label.err"
    (
        set +e
        "$EE_BINARY" remember \
            "Read pool writer $label for $RUN_ID commits during context readers." \
            --workspace "$EPIC_WORKSPACE" \
            --level procedural \
            --kind fact \
            --no-propose-candidates \
            --json >"$out_file" 2>"$err_file"
        rc=$?
        set -e
        printf 'rc=%s\n' "$rc" >"$ARTIFACT_DIR/$label.meta"
    ) &
    SPAWNED_PID="$!"
}

assert_context_response() {
    local label="${1:?label required}"
    local out_file="$ARTIFACT_DIR/$label.out"
    local err_file="$ARTIFACT_DIR/$label.err"
    local meta_file="$ARTIFACT_DIR/$label.meta"
    local stdout stderr rc elapsed pool_size pack_hash success
    stdout="$(cat "$out_file" 2>/dev/null || true)"
    stderr="$(cat "$err_file" 2>/dev/null || true)"
    rc="$(sed -n 's/^rc=//p' "$meta_file" 2>/dev/null | head -n 1)"
    elapsed="$(sed -n 's/^elapsed_ms=//p' "$meta_file" 2>/dev/null | head -n 1)"
    pool_size="$(sed -n 's/^pool_size=//p' "$meta_file" 2>/dev/null | head -n 1)"
    success="$(printf '%s' "$stdout" | jq -r '.success // false' 2>/dev/null || printf 'false')"
    pack_hash="$(printf '%s' "$stdout" | jq -r '.data.pack.hash // empty' 2>/dev/null || true)"

    e2e_log_assert_eq "${rc:-missing}" "0" "${label}_context_exit_zero"
    e2e_log_assert_eq "$success" "true" "${label}_context_success"
    assert_jq_nonempty "$stdout" '.data.pack.hash // empty' "${label}_context_pack_hash"
    case "$(printf '%s' "$stderr" | tr '[:upper:]' '[:lower:]')" in
        *"database is locked"*|*"sqlite_busy"*|*"database locked"*|*"panicked"*)
            e2e_log_assert_eq "$stderr" "no raw lock/panic stderr" "${label}_context_stderr_clean"
            ;;
        *)
            e2e_log_assert_eq "clean" "clean" "${label}_context_stderr_clean"
            ;;
    esac
    read_pool_step "context" "$success" "$label" "${elapsed:-0}" "${pool_size:-}" "$pack_hash"
}

assert_writer_response() {
    local label="${1:?label required}"
    local out_file="$ARTIFACT_DIR/$label.out"
    local err_file="$ARTIFACT_DIR/$label.err"
    local meta_file="$ARTIFACT_DIR/$label.meta"
    local stdout stderr rc code
    stdout="$(cat "$out_file" 2>/dev/null || true)"
    stderr="$(cat "$err_file" 2>/dev/null || true)"
    rc="$(sed -n 's/^rc=//p' "$meta_file" 2>/dev/null | head -n 1)"

    if [ "${rc:-1}" -eq 0 ] 2>/dev/null; then
        e2e_log_assert_eq "$(printf '%s' "$stdout" | jq -r '.success // false' 2>/dev/null)" \
            "true" "${label}_writer_success"
    else
        code="$(printf '%s' "$stdout" | jq -r '
            (.error.code // empty),
            (.data.degraded[]?.code // empty),
            (.degraded[]?.code // empty)
        ' 2>/dev/null | sed '/^$/d' | head -n 1)"
        case "$code" in
            write_queue_full|write_spool_backpressure|write_owner_busy)
                e2e_log_assert_eq "structured_contention" "structured_contention" \
                    "${label}_writer_structured_contention"
                ;;
            *)
                e2e_log_assert_eq "exit=$rc code=${code:-none} stderr=$stderr" \
                    "success_or_structured_contention" "${label}_writer_result"
                ;;
        esac
    fi
}

p50_for_batch() {
    local prefix="${1:?prefix required}"
    python3 - "$ARTIFACT_DIR" "$prefix" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
prefix = sys.argv[2]
values = []
for path in sorted(root.glob(f"{prefix}-context-*.meta")):
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("elapsed_ms="):
            values.append(int(line.split("=", 1)[1]))
if not values:
    print(0)
else:
    values.sort()
    print(values[len(values) // 2])
PY
}

run_batch() {
    local prefix="${1:?prefix required}"
    local pool_size="${2:?pool size required}"
    local pids=()
    local writer_pids=()
    local i
    write_read_pool_config "$pool_size"

    for i in $(seq 1 "$READ_POOL_CONTEXTS"); do
        spawn_context_reader "$prefix-context-$i" "$pool_size"
        pids+=("$SPAWNED_PID")
    done

    sleep 0.1
    for i in $(seq 1 "$READ_POOL_WRITERS"); do
        spawn_writer "$prefix-writer-$i"
        writer_pids+=("$SPAWNED_PID")
    done

    for pid in "${pids[@]}"; do
        wait "$pid" || true
    done
    for pid in "${writer_pids[@]}"; do
        wait "$pid" || true
    done

    for i in $(seq 1 "$READ_POOL_CONTEXTS"); do
        assert_context_response "$prefix-context-$i"
    done
    for i in $(seq 1 "$READ_POOL_WRITERS"); do
        assert_writer_response "$prefix-writer-$i"
    done

    local p50
    p50="$(p50_for_batch "$prefix")"
    e2e_log_assert_num "$p50" -gt 0 "${prefix}_p50_positive"
    read_pool_step "perf" "true" "${prefix}_p50_ms=$p50" "$p50" "$pool_size"
    printf '%s\n' "$p50"
}

seed_read_pool_corpus

P50_SINGLE="$(run_batch "pool1" 1)"
P50_EIGHT="$(run_batch "pool8" 8)"

read_pool_step "summary" "true" "pool1_p50=$P50_SINGLE pool8_p50=$P50_EIGHT" "0" ""

if [ "$READ_POOL_ENFORCE_SPEEDUP" = "1" ]; then
    read_pool_step "perf_gate" "false" \
        "process_fanout_cli cannot prove process-local read-pool speedup; use an in-process fanout benchmark or harness" \
        "0" ""
    e2e_log_assert_eq "$READ_POOL_HARNESS_MODE" "in_process_pool_benchmark" \
        "read_pool_strict_speedup_requires_in_process_harness"
else
    e2e_log_assert_eq "recorded" "recorded" "read_pool_perf_recorded_without_enforcement"
fi
