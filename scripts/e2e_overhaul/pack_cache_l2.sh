#!/usr/bin/env bash
# L2 pack cache e2e harness.
#
# Covers the canonical public ee pack JSON path with a host-local L2 cache root:
# one fresh assembly seeds the cache, three identical follow-up requests must
# replay byte-identical JSON, and corruption/unavailable cache states must
# degrade while preserving a successful pack response. The deprecated context
# alias is checked separately for pack parity and confirmed not to populate L2.

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
    "L2 pack cache e2e memory: identical pack requests should reuse cached JSON responses." \
    --json >/dev/null 2>&1 || true
ee_workspace remember \
    --level semantic \
    --kind note \
    --no-auto-link \
    "L2 pack cache e2e memory: corruption and unavailable cache roots must degrade without failing pack." \
    --json >/dev/null 2>&1 || true

QUERY="L2 pack cache e2e identical pack cache replay"
ARTIFACT_DIR="$EPIC_WORKSPACE/pack-cache-l2-artifacts"
mkdir -p "$ARTIFACT_DIR"

cache_file_count() {
    local cache_root="${1:-$EE_L2_PACK_CACHE_DIR}"
    find "$cache_root" -type f -name '*.json' 2>/dev/null | wc -l | tr -d ' '
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
ee_workspace pack "$QUERY" --max-tokens 1200 --json >"$SEED_JSON_PATH"
SEED_JSON="$(cat "$SEED_JSON_PATH")"
assert_jq "$SEED_JSON" '.success' "true" "pack_cache_l2_seed_pack_success"
assert_jq_nonempty "$SEED_JSON" '.schema // empty' "pack_cache_l2_seed_schema"
assert_jq_nonempty "$SEED_JSON" '.data.pack.hash // empty' "pack_cache_l2_seed_pack_hash"

INITIAL_CACHE_FILES="$(cache_file_count)"
e2e_log_assert_num "${INITIAL_CACHE_FILES:-0}" -ge 1 "pack_cache_l2_seed_writes_cache_file"

HIT_PIDS=""
for index in 1 2 3; do
    (
        "$EE_BINARY" pack "$QUERY" \
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
    assert_jq "$HIT_JSON" '.success' "true" "pack_cache_l2_hit_${index}_pack_success"
    if cmp -s "$SEED_JSON_PATH" "$HIT_PATH"; then
        e2e_log_assert_eq "byte_identical" "byte_identical" "pack_cache_l2_hit_${index}_matches_seed"
    else
        e2e_log_assert_eq "different_json" "byte_identical" "pack_cache_l2_hit_${index}_matches_seed" || true
    fi
done

POST_HIT_CACHE_FILES="$(cache_file_count)"
e2e_log_assert_num "${POST_HIT_CACHE_FILES:-0}" -ge 1 "pack_cache_l2_hits_keep_cache_populated"

CORRUPT_PATH="$(
    find "$EE_L2_PACK_CACHE_DIR" -type f -name '*.json' 2>/dev/null \
        | LC_ALL=C sort \
        | sed -n '1p'
)"
if [ -n "$CORRUPT_PATH" ]; then
    printf '{"schema":"corrupt-pack-cache-l2-entry"}\n' >"$CORRUPT_PATH"
    CORRUPTION_JSON="$(ee_workspace pack "$QUERY" --max-tokens 1200 --json 2>/dev/null || true)"
    assert_jq "$CORRUPTION_JSON" '.success' "true" "pack_cache_l2_corruption_pack_success"
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
        "$EE_BINARY" pack "$QUERY" \
            --workspace "$EPIC_WORKSPACE" \
            --max-tokens 1200 \
            --json 2>/dev/null || true
)"
assert_jq "$UNAVAILABLE_JSON" '.success' "true" "pack_cache_l2_unavailable_pack_success"
if degraded_has_code "$UNAVAILABLE_JSON" "l2_pack_cache_unavailable"; then
    e2e_log_assert_eq "l2_pack_cache_unavailable" "l2_pack_cache_unavailable" "pack_cache_l2_unavailable_degraded_code"
else
    e2e_log_assert_eq "missing" "l2_pack_cache_unavailable" "pack_cache_l2_unavailable_degraded_code" || true
fi

ALIAS_CACHE_ROOT="$EPIC_WORKSPACE/context-alias-cache"
ALIAS_JSON_PATH="$ARTIFACT_DIR/context_alias.json"
mkdir -p "$ALIAS_CACHE_ROOT"
EE_L2_PACK_CACHE_DIR="$ALIAS_CACHE_ROOT" \
    "$EE_BINARY" context "$QUERY" \
        --workspace "$EPIC_WORKSPACE" \
        --max-tokens 1200 \
        --json >"$ALIAS_JSON_PATH" 2>"$ARTIFACT_DIR/context_alias.stderr"
ALIAS_JSON="$(cat "$ALIAS_JSON_PATH")"
assert_jq "$ALIAS_JSON" '.success' "true" "pack_cache_l2_context_alias_success"
if degraded_has_code "$ALIAS_JSON" "deprecated_alias"; then
    e2e_log_assert_eq "deprecated_alias" "deprecated_alias" "pack_cache_l2_context_alias_degraded_code"
else
    e2e_log_assert_eq "missing" "deprecated_alias" "pack_cache_l2_context_alias_degraded_code" || true
fi
SEED_PACK_HASH="$(jq -r '.data.pack.hash // empty' "$SEED_JSON_PATH")"
ALIAS_PACK_HASH="$(jq -r '.data.pack.hash // empty' "$ALIAS_JSON_PATH")"
e2e_log_assert_eq "$ALIAS_PACK_HASH" "$SEED_PACK_HASH" "pack_cache_l2_context_alias_pack_hash_parity"
ALIAS_CACHE_FILES="$(cache_file_count "$ALIAS_CACHE_ROOT")"
e2e_log_assert_num "${ALIAS_CACHE_FILES:-0}" -eq 0 "pack_cache_l2_context_alias_writes_no_cache_file"

ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
e2e_log_note "pack_cache_l2_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} cache_files=${POST_HIT_CACHE_FILES:-0}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
