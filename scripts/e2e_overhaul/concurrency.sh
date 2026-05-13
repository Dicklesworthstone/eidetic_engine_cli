#!/usr/bin/env bash
# J3 — L1 concurrent write contention / write-spool backpressure e2e driver.
#
# Exercises the user-visible concurrency contract with real `ee` child
# processes:
#   - two concurrent `remember` writers serialize without data loss
#   - a 10-writer burst succeeds or reports structured write backpressure
#   - search succeeds while the burst is in flight
#   - a killed writer leaves the workspace usable for the next durable write
#   - `diag write-spool` emits the structured backpressure envelope

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "l1_concurrency"

RUN_SUFFIX="$(printf '%s' "$$" | tr '0123456789' 'abcdefghij')"
RUN_ID="lone${RUN_SUFFIX}"
ARTIFACT_DIR="$EPIC_WORKSPACE/l1-concurrency-artifacts"
mkdir -p "$ARTIFACT_DIR"
SPAWNED_PID=""

concurrency_step() {
    local step_id="${1:?step id required}"
    local expected_count="${2:-}"
    local observed_count="${3:-}"
    local status="${4:?status required}"
    _e2e_emit_event "concurrency_e2e_step" \
        "step_id" "$step_id" \
        "expected_count" "$expected_count" \
        "observed_count" "$observed_count" \
        "status" "$status"
}

spawn_remember_writer() {
    local label="${1:?label required}"
    local content="${2:?content required}"
    local out_file="${3:?stdout file required}"
    local err_file="${4:?stderr file required}"
    "$EE_BINARY" remember "$content" \
        --workspace "$EPIC_WORKSPACE" \
        --level procedural \
        --kind fact \
        --no-propose-candidates \
        --json >"$out_file" 2>"$err_file" &
    SPAWNED_PID="$!"
    concurrency_step "spawn_$label" "started" "$SPAWNED_PID" "ok"
}

assert_writer_result() {
    local label="${1:?label required}"
    local status="${2:?process status required}"
    local out_file="${3:?stdout file required}"
    local err_file="${4:?stderr file required}"
    local stdout stderr code
    stdout="$(cat "$out_file" 2>/dev/null || true)"
    stderr="$(cat "$err_file" 2>/dev/null || true)"

    if [ "$status" -eq 0 ]; then
        e2e_log_assert_eq "$(printf '%s' "$stdout" | jq -r '.success // false' 2>/dev/null)" \
            "true" "${label}_writer_success"
        assert_jq_nonempty "$stdout" '.data.memory_id // empty' "${label}_writer_memory_id"
    else
        code="$(printf '%s' "$stdout" | jq -r '
            (.error.code // empty),
            (.data.degraded[]?.code // empty),
            (.degraded[]?.code // empty)
        ' 2>/dev/null | sed '/^$/d' | head -n 1)"
        case "$code" in
            write_queue_full|write_spool_backpressure|write_owner_busy)
                e2e_log_assert_eq "structured_backpressure" "structured_backpressure" \
                    "${label}_writer_structured_contention"
                ;;
            *)
                e2e_log_assert_eq "exit=$status code=${code:-none} stderr=$stderr" \
                    "success_or_structured_contention" "${label}_writer_result"
                ;;
        esac
    fi

    case "$(printf '%s' "$stderr" | tr '[:upper:]' '[:lower:]')" in
        *"database is locked"*|*"sqlite_busy"*|*"database locked"*|*"panicked"*)
            e2e_log_assert_eq "$stderr" "no raw lock/panic stderr" "${label}_writer_stderr_clean"
            ;;
        *)
            e2e_log_assert_eq "clean" "clean" "${label}_writer_stderr_clean"
            ;;
    esac
}

memory_count_for_marker() {
    local marker="${1:?marker required}"
    local json
    json=$("$EE_BINARY" memory list \
        --workspace "$EPIC_WORKSPACE" \
        --json 2>/dev/null || true)
    printf '%s' "$json" | jq -r --arg marker "$marker" '
        [.data.memories[]? | select((.content // "") | contains($marker))] | length
    ' 2>/dev/null || echo 0
}

# ---------------------------------------------------------------------------
# Two-writer happy path.
# ---------------------------------------------------------------------------
TWO_MARKER="L1 two writer $RUN_ID"
TWO_PIDS=()
for index in 0 1; do
    spawn_remember_writer \
        "two_$index" \
        "$TWO_MARKER writer $index must persist" \
        "$ARTIFACT_DIR/two-$index.out" \
        "$ARTIFACT_DIR/two-$index.err"
    TWO_PIDS+=("$SPAWNED_PID")
done

for index in 0 1; do
    if wait "${TWO_PIDS[$index]}"; then
        status=0
    else
        status=$?
    fi
    assert_writer_result "l1_two_writer_$index" "$status" \
        "$ARTIFACT_DIR/two-$index.out" "$ARTIFACT_DIR/two-$index.err"
done

TWO_COUNT="$(memory_count_for_marker "$TWO_MARKER")"
e2e_log_assert_eq "$TWO_COUNT" "2" "l1_two_writer_no_data_loss"
concurrency_step "two_writer_happy_path" "2" "$TWO_COUNT" "ok"

# ---------------------------------------------------------------------------
# Heavy burst with reader in flight.
# ---------------------------------------------------------------------------
BURST_MARKER="L1 burst writer $RUN_ID"
BURST_PIDS=()
for index in $(seq 0 9); do
    spawn_remember_writer \
        "burst_$index" \
        "$BURST_MARKER writer $index durable row" \
        "$ARTIFACT_DIR/burst-$index.out" \
        "$ARTIFACT_DIR/burst-$index.err"
    BURST_PIDS+=("$SPAWNED_PID")
done

SEARCH_JSON=$("$EE_BINARY" search "$TWO_MARKER" \
    --workspace "$EPIC_WORKSPACE" \
    --relevance-floor 0.0 \
    --json 2>"$ARTIFACT_DIR/search-during-burst.err" || true)
e2e_log_assert_eq "$(printf '%s' "$SEARCH_JSON" | jq -r '.success // false' 2>/dev/null)" \
    "true" "l1_reader_during_writes_search_succeeds"
concurrency_step "reader_during_burst" "success" \
    "$(printf '%s' "$SEARCH_JSON" | jq -r '.success // false' 2>/dev/null)" "ok"

BURST_SUCCEEDED=0
BURST_STRUCTURED=0
for index in $(seq 0 9); do
    if wait "${BURST_PIDS[$index]}"; then
        status=0
        BURST_SUCCEEDED=$((BURST_SUCCEEDED + 1))
    else
        status=$?
    fi
    assert_writer_result "l1_burst_writer_$index" "$status" \
        "$ARTIFACT_DIR/burst-$index.out" "$ARTIFACT_DIR/burst-$index.err"
    if [ "$status" -ne 0 ]; then
        BURST_STRUCTURED=$((BURST_STRUCTURED + 1))
    fi
done

BURST_COUNT="$(memory_count_for_marker "$BURST_MARKER")"
e2e_log_assert_num "$BURST_COUNT" -ge "$BURST_SUCCEEDED" \
    "l1_burst_successes_are_persisted"
concurrency_step "ten_writer_burst" "10" "$((BURST_SUCCEEDED + BURST_STRUCTURED))" "ok"

# ---------------------------------------------------------------------------
# Structured write-spool backpressure envelope.
# ---------------------------------------------------------------------------
SPOOL_JSON=$(ee_workspace diag write-spool \
    --max-pending 1 \
    --enqueue 2 \
    --json 2>/dev/null || true)
assert_jq "$SPOOL_JSON" '.data.degraded[0].code // empty' \
    "write_spool_backpressure" "l1_write_spool_backpressure_code"
assert_jq "$SPOOL_JSON" '.data.degraded[0].reason // empty' \
    "queue_depth" "l1_write_spool_backpressure_reason"
assert_jq "$SPOOL_JSON" '.data.degraded[0].repair // empty' \
    "ee daemon status --json" "l1_write_spool_backpressure_repair"
assert_jq "$SPOOL_JSON" '.data.degraded[0].queueDepth // empty' \
    "1" "l1_write_spool_backpressure_queue_depth"
assert_jq "$SPOOL_JSON" \
    '.data.degraded | map(select(.code == "write_queue_full" and .severity == "low")) | length > 0' \
    "true" "l1_write_queue_full_alias_code"
assert_jq "$SPOOL_JSON" \
    '.data.degraded[] | select(.code == "write_queue_full") | .recovery[0].hint // empty' \
    "Retry with --backoff-ms 100" "l1_write_queue_full_backoff_hint"
assert_jq "$SPOOL_JSON" \
    '.data.degraded[] | select(.code == "write_queue_full") | .recovery[1].key // empty' \
    "writeSpool.queueCap" "l1_write_queue_full_config_hint"
concurrency_step "write_spool_backpressure" "write_spool_backpressure" \
    "$(printf '%s' "$SPOOL_JSON" | jq -r '.data.degraded[0].code // empty' 2>/dev/null)" "ok"

# ---------------------------------------------------------------------------
# Kill a writer and prove the next durable write can recover/use the workspace.
# ---------------------------------------------------------------------------
CRASH_MARKER="L1 killed writer $RUN_ID"
"$EE_BINARY" remember "$CRASH_MARKER maybe persisted before kill" \
    --workspace "$EPIC_WORKSPACE" \
    --level procedural \
    --kind fact \
    --no-propose-candidates \
    --json >"$ARTIFACT_DIR/crash.out" 2>"$ARTIFACT_DIR/crash.err" &
CRASH_PID=$!
REPLAY_STATE_FILE="$EPIC_WORKSPACE/.ee/write-spool/recovery-state.json"
for _attempt in $(seq 1 200); do
    if [ -f "$REPLAY_STATE_FILE" ] \
        && jq -e '.state == "uncommitted_write_replay_required"' \
            "$REPLAY_STATE_FILE" >/dev/null 2>&1; then
        break
    fi
    if ! kill -0 "$CRASH_PID" 2>/dev/null; then
        break
    fi
    sleep 0.005
done
kill "$CRASH_PID" 2>/dev/null || true
wait "$CRASH_PID" >/dev/null 2>&1 || true

PRE_RECOVERY_STATUS_JSON=$(ee_workspace status --json 2>/dev/null || true)
e2e_log_assert_eq "$(printf '%s' "$PRE_RECOVERY_STATUS_JSON" | jq -r '.success // false' 2>/dev/null)" \
    "true" "l1_status_after_killed_writer_reports"
assert_jq "$PRE_RECOVERY_STATUS_JSON" '.data.posture.workspace.storage.status // empty' \
    "degraded_recoverable" "l1_post_kill_storage_degraded_recoverable"
assert_jq "$PRE_RECOVERY_STATUS_JSON" '.data.posture.workspace.storage.reason // empty' \
    "uncommitted_write_replay_required" "l1_post_kill_storage_replay_reason"

RECOVERY_JSON=$(ee_workspace remember \
    "L1 post-kill recovery $RUN_ID durable write succeeds" \
    --level procedural \
    --kind fact \
    --no-propose-candidates \
    --json 2>/dev/null || true)
e2e_log_assert_eq "$(printf '%s' "$RECOVERY_JSON" | jq -r '.success // false' 2>/dev/null)" \
    "true" "l1_post_kill_remember_succeeds"

CRASH_COUNT="$(memory_count_for_marker "$CRASH_MARKER")"
e2e_log_assert_num "$CRASH_COUNT" -le 1 "l1_killed_writer_no_duplicate_partial_rows"
STATUS_JSON=$(ee_workspace status --json 2>/dev/null || true)
e2e_log_assert_eq "$(printf '%s' "$STATUS_JSON" | jq -r '.success // false' 2>/dev/null)" \
    "true" "l1_status_after_killed_writer_succeeds"
assert_jq "$STATUS_JSON" '.data.posture.workspace.storage.reason // empty' \
    "" "l1_post_recovery_storage_replay_reason_cleared"
concurrency_step "killed_writer_recovery" "0_or_1" "$CRASH_COUNT" "ok"
