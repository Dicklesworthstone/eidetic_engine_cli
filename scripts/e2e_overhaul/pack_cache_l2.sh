#!/usr/bin/env bash
# L2 pack cache e2e harness.
#
# Covers the public ee context JSON path with a host-local L2 cache root:
# one fresh assembly seeds the cache, three identical follow-up requests must
# replay byte-identical JSON, and corruption/unavailable cache states must
# degrade while preserving a successful context response.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq

START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "pack_cache_l2"

export EE_L2_PACK_CACHE_DIR="$EPIC_WORKSPACE/pack-l2-cache"
export EE_L2_PACK_CACHE_BYTES="${EE_L2_PACK_CACHE_BYTES:-16777216}"
export EE_L2_PACK_CACHE_DISABLE=0

seed_corpus

ee_workspace remember \
    --level procedural \
    --kind rule \
    --no-auto-link \
    "L2 pack cache e2e memory: identical context requests should reuse cached JSON responses." \
    --json >/dev/null 2>&1 || true
ee_workspace remember \
    --level semantic \
    --kind note \
    --no-auto-link \
    "L2 pack cache e2e memory: corruption and unavailable cache roots must degrade without failing context." \
    --json >/dev/null 2>&1 || true

QUERY="L2 pack cache e2e identical context cache replay"
ARTIFACT_DIR="$EPIC_WORKSPACE/pack-cache-l2-artifacts"
mkdir -p "$ARTIFACT_DIR"

cache_file_count() {
    find "$EE_L2_PACK_CACHE_DIR" -type f -name '*.json' 2>/dev/null | wc -l | tr -d ' '
}

degraded_has_code() {
    local json="${1:?json required}"
    local code="${2:?code required}"
    printf '%s' "$json" \
        | jq -e --arg code "$code" '
            [
              .degraded[]?.code,
              .data.degraded[]?.code
            ] | index($code) != null
        ' >/dev/null 2>&1
}

SEED_JSON_PATH="$ARTIFACT_DIR/fresh_seed.json"
ee_workspace context "$QUERY" --max-tokens 1200 --json >"$SEED_JSON_PATH"
SEED_JSON="$(cat "$SEED_JSON_PATH")"
assert_jq "$SEED_JSON" '.success' "true" "pack_cache_l2_seed_context_success"
assert_jq_nonempty "$SEED_JSON" '.schema // empty' "pack_cache_l2_seed_schema"

INITIAL_CACHE_FILES="$(cache_file_count)"
e2e_log_assert_num "${INITIAL_CACHE_FILES:-0}" -ge 1 "pack_cache_l2_seed_writes_cache_file"

HIT_PIDS=""
for index in 1 2 3; do
    (
        "$EE_BINARY" context "$QUERY" \
            --workspace "$EPIC_WORKSPACE" \
            --max-tokens 1200 \
            --json >"$ARTIFACT_DIR/l2_hit_${index}.json" \
            2>"$ARTIFACT_DIR/l2_hit_${index}.stderr"
    ) &
    HIT_PIDS="$HIT_PIDS $!"
done

HIT_FAILURES=0
for pid in $HIT_PIDS; do
    if ! wait "$pid"; then
        HIT_FAILURES=$((HIT_FAILURES + 1))
    fi
done
e2e_log_assert_num "$HIT_FAILURES" -eq 0 "pack_cache_l2_parallel_hit_processes_exit_zero"

for index in 1 2 3; do
    HIT_PATH="$ARTIFACT_DIR/l2_hit_${index}.json"
    HIT_JSON="$(cat "$HIT_PATH")"
    assert_jq "$HIT_JSON" '.success' "true" "pack_cache_l2_hit_${index}_context_success"
    if cmp -s "$SEED_JSON_PATH" "$HIT_PATH"; then
        e2e_log_assert_eq "byte_identical" "byte_identical" "pack_cache_l2_hit_${index}_matches_seed"
    else
        e2e_log_assert_eq "different_json" "byte_identical" "pack_cache_l2_hit_${index}_matches_seed" || true
    fi
done

POST_HIT_CACHE_FILES="$(cache_file_count)"
e2e_log_assert_num "${POST_HIT_CACHE_FILES:-0}" -ge 1 "pack_cache_l2_hits_keep_cache_populated"

CORRUPT_PATH="$(find "$EE_L2_PACK_CACHE_DIR" -type f -name '*.json' 2>/dev/null | head -n 1)"
if [ -n "$CORRUPT_PATH" ]; then
    printf '{"schema":"corrupt-pack-cache-l2-entry"}\n' >"$CORRUPT_PATH"
    CORRUPTION_JSON="$(ee_workspace context "$QUERY" --max-tokens 1200 --json 2>/dev/null || true)"
    assert_jq "$CORRUPTION_JSON" '.success' "true" "pack_cache_l2_corruption_context_success"
    if degraded_has_code "$CORRUPTION_JSON" "l2_pack_cache_corruption"; then
        e2e_log_assert_eq "l2_pack_cache_corruption" "l2_pack_cache_corruption" "pack_cache_l2_corruption_degraded_code"
    else
        e2e_log_assert_eq "missing" "l2_pack_cache_corruption" "pack_cache_l2_corruption_degraded_code" || true
    fi
else
    e2e_log_assert_eq "cache_entry_found" "cache_entry_missing" "pack_cache_l2_corruption_fixture_entry"
fi

UNAVAILABLE_ROOT_FILE="$EPIC_WORKSPACE/l2-cache-root-is-file"
printf 'not a directory\n' >"$UNAVAILABLE_ROOT_FILE"
UNAVAILABLE_JSON="$(
    EE_L2_PACK_CACHE_DIR="$UNAVAILABLE_ROOT_FILE" \
        "$EE_BINARY" context "$QUERY" \
            --workspace "$EPIC_WORKSPACE" \
            --max-tokens 1200 \
            --json 2>/dev/null || true
)"
assert_jq "$UNAVAILABLE_JSON" '.success' "true" "pack_cache_l2_unavailable_context_success"
if degraded_has_code "$UNAVAILABLE_JSON" "l2_pack_cache_unavailable"; then
    e2e_log_assert_eq "l2_pack_cache_unavailable" "l2_pack_cache_unavailable" "pack_cache_l2_unavailable_degraded_code"
else
    e2e_log_assert_eq "missing" "l2_pack_cache_unavailable" "pack_cache_l2_unavailable_degraded_code" || true
fi

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "pack_cache_l2_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} cache_files=${POST_HIT_CACHE_FILES:-0}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
