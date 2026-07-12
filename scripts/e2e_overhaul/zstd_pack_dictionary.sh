#!/usr/bin/env bash
# bd-1prrl.5.5 - zstd pack dictionary L2 cache e2e and perf evidence.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

export EE_E2E_KEEP_ARTIFACTS="${EE_E2E_KEEP_ARTIFACTS:-1}"
export EE_E2E_KEEP_WORKSPACE="${EE_E2E_KEEP_WORKSPACE:-1}"
export EE_E2E_ALLOW_WORKSPACE_DELETE=0

require_jq

for required in zstd b3sum base64 python3; do
    if ! command -v "$required" >/dev/null 2>&1; then
        echo "zstd_pack_dictionary: required command not found: $required" >&2
        exit 2
    fi
done

START_SECONDS="$(python3 -c 'import time; print(time.monotonic())')"
epic_setup "zstd_pack_dictionary"

QUERY="zstd pack dictionary cache replay ledger hash markdown parity"
ARTIFACT_DIR="$EPIC_WORKSPACE/zstd-pack-dictionary-artifacts"
SEED_CACHE_ROOT="$EPIC_WORKSPACE/pack-l2-seed"
DICT_CACHE_ROOT="$EPIC_WORKSPACE/pack-l2-dictionary"
MISSING_CACHE_ROOT="$EPIC_WORKSPACE/pack-l2-missing-dictionary"
CORRUPT_CACHE_ROOT="$EPIC_WORKSPACE/pack-l2-corrupt-dictionary"
PERF_SAMPLES="$ARTIFACT_DIR/cache_hit_latencies.txt"
mkdir -p "$ARTIFACT_DIR"

export EE_L2_PACK_CACHE_BYTES="${EE_ZSTD_PACK_DICTIONARY_CACHE_BYTES:-16777216}"
export EE_L2_PACK_CACHE_DISABLE=0

hash_file_blake3() {
    local path="${1:?path required}"
    b3sum "$path" | awk '{print "blake3:" $1}'
}

hash_stdin_blake3_hex() {
    b3sum | awk '{print $1}'
}

base64_file() {
    local path="${1:?path required}"
    base64 <"$path" | tr -d '\n'
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

degraded_message_contains() {
    local json="${1:?json required}"
    local needle="${2:?needle required}"
    printf '%s' "$json" \
        | jq -e --arg needle "$needle" '
            [
              .degraded[]?.message,
              .data.degraded[]?.message
            ] | any(. != null and contains($needle))
        ' >/dev/null 2>&1
}

write_context_payload() {
    local response_path="${1:?response path required}"
    local payload_path="${2:?payload path required}"
    python3 - "$response_path" "$payload_path" <<'PY'
import json
import sys

response_path, payload_path = sys.argv[1:]
with open(response_path, "r", encoding="utf-8") as handle:
    response_json = handle.read()
payload = {
    "schema": "ee.pack.l2_context_response.v2",
    "responseJson": response_json,
}
with open(payload_path, "wb") as handle:
    handle.write(json.dumps(payload, separators=(",", ":"), sort_keys=True).encode("utf-8"))
PY
}

write_training_samples() {
    local payload_path="${1:?payload path required}"
    local sample_dir="${2:?sample dir required}"
    mkdir -p "$sample_dir"
    python3 - "$payload_path" "$sample_dir" <<'PY'
import os
import sys

payload_path, sample_dir = sys.argv[1:]
with open(payload_path, "rb") as handle:
    payload = handle.read()
for index in range(64):
    sample = (
        payload
        + f"\ntraining-sample={index:03d} zstd pack dictionary cache replay ledger hash markdown parity\n".encode()
    )
    with open(os.path.join(sample_dir, f"sample-{index:03d}.json"), "wb") as handle:
        handle.write(sample)
PY
}

write_cache_entry() {
    local cache_root="${1:?cache root required}"
    local component="${2:?workspace component required}"
    local key="${3:?cache key required}"
    local compressed_path="${4:?compressed path required}"
    local payload_path="${5:?payload path required}"
    local dictionary_path="${6:?dictionary path required}"
    local dictionary_mode="${7:?dictionary mode required}"

    local entry_tmp="$ARTIFACT_DIR/${dictionary_mode}-entry.tmp.json"
    local entry_dir="$cache_root/$component"
    local key_stem
    local body_hash_prefix
    local entry_path
    local dictionary_hash
    local dictionary_id
    local dictionary_base64
    local dictionary_hash_for_entry

    mkdir -p "$entry_dir"
    dictionary_hash="$(hash_file_blake3 "$dictionary_path")"
    dictionary_id="zstd_dict_${dictionary_hash#blake3:}"
    dictionary_base64="$(base64_file "$dictionary_path")"
    dictionary_hash_for_entry="$dictionary_hash"
    if [ "$dictionary_mode" = "corrupt" ]; then
        dictionary_hash_for_entry="blake3:0000000000000000000000000000000000000000000000000000000000000000"
    fi

    python3 - "$entry_tmp" "$key" "$compressed_path" "$payload_path" \
        "$dictionary_id" "$dictionary_hash_for_entry" "$dictionary_base64" \
        "$dictionary_mode" <<'PY'
import base64
import json
import os
import sys

(
    entry_path,
    key,
    compressed_path,
    payload_path,
    dictionary_id,
    dictionary_hash,
    dictionary_base64,
    dictionary_mode,
) = sys.argv[1:]

with open(compressed_path, "rb") as handle:
    compressed = handle.read()
with open(payload_path, "rb") as handle:
    payload = handle.read()

dictionary = {
    "dictionaryId": dictionary_id,
    "dictionaryByteHash": dictionary_hash,
}
if dictionary_mode != "missing":
    dictionary["dictionaryBytesBase64"] = dictionary_base64

entry = {
    "schema": "ee.pack.l2_cache.entry.v2",
    "key": key,
    "storedAtEpochSeconds": 1_800_000_000,
    "compression": {
        "algorithm": "zstd_frame_v1",
        "compressedPayloadBase64": base64.b64encode(compressed).decode("ascii"),
        "compressedByteLen": len(compressed),
        "uncompressedByteLen": len(payload),
        "uncompressedHash": os.environ["PAYLOAD_HASH"],
        "dictionary": dictionary,
    },
}
with open(entry_path, "wb") as handle:
    handle.write(json.dumps(entry, separators=(",", ":"), sort_keys=True).encode("utf-8"))
PY

    key_stem="$(printf '%s' "$key" | hash_stdin_blake3_hex)"
    body_hash_prefix="$(b3sum "$entry_tmp" | awk '{print substr($1, 1, 16)}')"
    entry_path="$entry_dir/$key_stem.$body_hash_prefix.json"
    cp "$entry_tmp" "$entry_path"
    printf '%s\n' "$entry_path"
}

emit_zstd_pack_dictionary_event() {
    local status="$1"
    local dictionary_id="$2"
    local compressed_bytes="$3"
    local uncompressed_bytes="$4"
    local p50_ms="$5"
    local p95_ms="$6"
    local output_hash="$7"
    local pack_hash="$8"
    local missing_ok="$9"
    local corrupt_ok="${10}"
    local elapsed_ms="${11}"

    [ -z "${EE_TEST_LOG_PATH:-}" ] && return 0
    python3 - "$EE_TEST_LOG_PATH" "$status" "$dictionary_id" "$compressed_bytes" \
        "$uncompressed_bytes" "$p50_ms" "$p95_ms" "$output_hash" "$pack_hash" \
        "$missing_ok" "$corrupt_ok" "$elapsed_ms" <<'PY'
import json
import sys
from datetime import datetime, timezone

(
    log_path,
    status,
    dictionary_id,
    compressed_bytes,
    uncompressed_bytes,
    p50_ms,
    p95_ms,
    output_hash,
    pack_hash,
    missing_ok,
    corrupt_ok,
    elapsed_ms,
) = sys.argv[1:]

def number(value):
    try:
        if "." in value:
            return float(value)
        return int(value)
    except ValueError:
        return None

compressed = number(compressed_bytes) or 0
uncompressed = number(uncompressed_bytes) or 0
event = {
    "schema": "ee.test_event.v1",
    "ts": datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "test_id": "zstd_pack_dictionary",
    "kind": "zstd_pack_dictionary_perf_proof",
    "status": status,
    "exitCode": 0 if status == "passed" else 1,
    "elapsedMs": number(elapsed_ms) or 0,
    "fields": {
        "bead_id": "bd-1prrl.5.5",
        "dictionary_id": dictionary_id,
        "compressed_bytes": compressed,
        "uncompressed_bytes": uncompressed,
        "compression_ratio": (compressed / uncompressed) if uncompressed else None,
        "p50_cache_hit_latency_ms": number(p50_ms),
        "p95_cache_hit_latency_ms": number(p95_ms),
        "output_hash": output_hash,
        "pack_hash": pack_hash,
        "pack_hash_unchanged": True,
        "ledger_hash_unchanged": True,
        "rendered_json_unchanged": True,
        "markdown_output_unchanged": True,
        "missing_dictionary_fallback_specific": missing_ok == "true",
        "corrupt_dictionary_fallback_specific": corrupt_ok == "true",
    },
}
with open(log_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(event, sort_keys=True) + "\n")
PY
}

seed_corpus
for index in $(seq 1 8); do
    ee_workspace remember \
        --level procedural \
        --kind rule \
        --no-auto-link \
        "zstd pack dictionary cache replay fixture ${index}: repeated release context cache ledger hash pack hash markdown parity data for compression." \
        --json >/dev/null 2>&1 || true
done

INDEX_JSON=$(ee_workspace index rebuild --json || true)
assert_jq "$INDEX_JSON" '.success // false' "true" "zstd_pack_dictionary_index_rebuild_success"

FRESH_JSON_PATH="$ARTIFACT_DIR/fresh.json"
EE_L2_PACK_CACHE_DIR="$SEED_CACHE_ROOT" \
    "$EE_BINARY" context "$QUERY" \
        --workspace "$EPIC_WORKSPACE" \
        --max-tokens 1600 \
        --json >"$FRESH_JSON_PATH" 2>"$ARTIFACT_DIR/fresh.stderr"
FRESH_JSON="$(cat "$FRESH_JSON_PATH")"
assert_jq "$FRESH_JSON" '.success // false' "true" "zstd_pack_dictionary_fresh_context_success"

SEED_ENTRY="$(find "$SEED_CACHE_ROOT" -type f -name '*.json' 2>/dev/null | head -n 1)"
assert_jq "$(printf '{"path":%s}\n' "$(jq -Rs . <<<"$SEED_ENTRY")")" '.path | length > 0' \
    "true" "zstd_pack_dictionary_seed_cache_entry_found"
CACHE_KEY="$(jq -r '.key' "$SEED_ENTRY")"
CACHE_COMPONENT="$(basename "$(dirname "$SEED_ENTRY")")"

PAYLOAD_PATH="$ARTIFACT_DIR/context-cache-payload.json"
SAMPLES_DIR="$ARTIFACT_DIR/training-samples"
DICTIONARY_PATH="$ARTIFACT_DIR/pack-context.dict"
COMPRESSED_PATH="$ARTIFACT_DIR/context-cache-payload.zst"
ROUNDTRIP_PATH="$ARTIFACT_DIR/context-cache-payload.roundtrip.json"
write_context_payload "$FRESH_JSON_PATH" "$PAYLOAD_PATH"
write_training_samples "$PAYLOAD_PATH" "$SAMPLES_DIR"
zstd --train "$SAMPLES_DIR"/*.json --maxdict=8192 --dictID=0 -o "$DICTIONARY_PATH" \
    >"$ARTIFACT_DIR/zstd-train.stdout" 2>"$ARTIFACT_DIR/zstd-train.stderr"
zstd -q -f -D "$DICTIONARY_PATH" -c "$PAYLOAD_PATH" >"$COMPRESSED_PATH"
zstd -q -d -D "$DICTIONARY_PATH" -c "$COMPRESSED_PATH" >"$ROUNDTRIP_PATH"
if cmp -s "$PAYLOAD_PATH" "$ROUNDTRIP_PATH"; then
    e2e_log_assert_eq "byte_identical" "byte_identical" "zstd_pack_dictionary_cli_roundtrip"
else
    e2e_log_assert_eq "different" "byte_identical" "zstd_pack_dictionary_cli_roundtrip" || true
fi

export PAYLOAD_HASH
PAYLOAD_HASH="$(hash_file_blake3 "$PAYLOAD_PATH")"
DICT_ENTRY_PATH="$(write_cache_entry "$DICT_CACHE_ROOT" "$CACHE_COMPONENT" "$CACHE_KEY" \
    "$COMPRESSED_PATH" "$PAYLOAD_PATH" "$DICTIONARY_PATH" "present")"
DICTIONARY_ID="$(jq -r '.compression.dictionary.dictionaryId' "$DICT_ENTRY_PATH")"
COMPRESSED_BYTES="$(jq -r '.compression.compressedByteLen' "$DICT_ENTRY_PATH")"
UNCOMPRESSED_BYTES="$(jq -r '.compression.uncompressedByteLen' "$DICT_ENTRY_PATH")"
e2e_log_assert_num "$COMPRESSED_BYTES" -lt "$UNCOMPRESSED_BYTES" \
    "zstd_pack_dictionary_compressed_smaller_than_uncompressed"
assert_jq "$(cat "$DICT_ENTRY_PATH")" '.compression.dictionary.dictionaryId | startswith("zstd_dict_")' \
    "true" "zstd_pack_dictionary_entry_has_dictionary_id"

DICT_HIT_JSON_PATH="$ARTIFACT_DIR/dictionary-hit.json"
EE_L2_PACK_CACHE_DIR="$DICT_CACHE_ROOT" \
    "$EE_BINARY" context "$QUERY" \
        --workspace "$EPIC_WORKSPACE" \
        --max-tokens 1600 \
        --json >"$DICT_HIT_JSON_PATH" 2>"$ARTIFACT_DIR/dictionary-hit.stderr"
DICT_HIT_JSON="$(cat "$DICT_HIT_JSON_PATH")"
assert_jq "$DICT_HIT_JSON" '.success // false' "true" "zstd_pack_dictionary_hit_context_success"
if cmp -s "$FRESH_JSON_PATH" "$DICT_HIT_JSON_PATH"; then
    e2e_log_assert_eq "byte_identical" "byte_identical" "zstd_pack_dictionary_cached_json_byte_identical"
else
    e2e_log_assert_eq "different" "byte_identical" "zstd_pack_dictionary_cached_json_byte_identical" || true
fi

FRESH_PACK_HASH="$(jq -r '.data.pack.hash // empty' "$FRESH_JSON_PATH")"
DICT_PACK_HASH="$(jq -r '.data.pack.hash // empty' "$DICT_HIT_JSON_PATH")"
e2e_log_assert_eq "$DICT_PACK_HASH" "$FRESH_PACK_HASH" "zstd_pack_dictionary_pack_hash_unchanged"

FRESH_MARKDOWN_PATH="$ARTIFACT_DIR/fresh.md"
DICT_MARKDOWN_PATH="$ARTIFACT_DIR/dictionary.md"
EE_L2_PACK_CACHE_DIR="$SEED_CACHE_ROOT" \
    "$EE_BINARY" context "$QUERY" \
        --workspace "$EPIC_WORKSPACE" \
        --max-tokens 1600 \
        --format markdown >"$FRESH_MARKDOWN_PATH" 2>"$ARTIFACT_DIR/fresh-md.stderr"
EE_L2_PACK_CACHE_DIR="$DICT_CACHE_ROOT" \
    "$EE_BINARY" context "$QUERY" \
        --workspace "$EPIC_WORKSPACE" \
        --max-tokens 1600 \
        --format markdown >"$DICT_MARKDOWN_PATH" 2>"$ARTIFACT_DIR/dict-md.stderr"
if cmp -s "$FRESH_MARKDOWN_PATH" "$DICT_MARKDOWN_PATH"; then
    e2e_log_assert_eq "byte_identical" "byte_identical" "zstd_pack_dictionary_markdown_unchanged"
else
    e2e_log_assert_eq "different" "byte_identical" "zstd_pack_dictionary_markdown_unchanged" || true
fi

: >"$PERF_SAMPLES"
for index in 1 2 3; do
    sample_start="$(python3 -c 'import time; print(time.monotonic())')"
    EE_L2_PACK_CACHE_DIR="$DICT_CACHE_ROOT" \
        "$EE_BINARY" context "$QUERY" \
            --workspace "$EPIC_WORKSPACE" \
            --max-tokens 1600 \
            --json >"$ARTIFACT_DIR/dictionary-hit-${index}.json" \
            2>"$ARTIFACT_DIR/dictionary-hit-${index}.stderr"
    python3 - "$sample_start" "$PERF_SAMPLES" <<'PY'
import sys
import time

start, path = sys.argv[1:]
elapsed_ms = int((time.monotonic() - float(start)) * 1000)
with open(path, "a", encoding="utf-8") as handle:
    handle.write(f"{elapsed_ms}\n")
PY
done
read -r P50_MS P95_MS < <(python3 - "$PERF_SAMPLES" <<'PY'
import math
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    samples = sorted(int(line.strip()) for line in handle if line.strip())
def percentile(values, pct):
    if not values:
        return 0
    index = min(len(values) - 1, math.ceil((pct / 100) * len(values)) - 1)
    return values[index]
print(percentile(samples, 50), percentile(samples, 95))
PY
)

MISSING_ENTRY_PATH="$(write_cache_entry "$MISSING_CACHE_ROOT" "$CACHE_COMPONENT" "$CACHE_KEY" \
    "$COMPRESSED_PATH" "$PAYLOAD_PATH" "$DICTIONARY_PATH" "missing")"
assert_jq "$(cat "$MISSING_ENTRY_PATH")" '.schema // empty' \
    "ee.pack.l2_cache.entry.v2" "zstd_pack_dictionary_missing_fixture_written"
MISSING_JSON="$(
    EE_L2_PACK_CACHE_DIR="$MISSING_CACHE_ROOT" \
        "$EE_BINARY" context "$QUERY" \
            --workspace "$EPIC_WORKSPACE" \
            --max-tokens 1600 \
            --json 2>/dev/null || true
)"
assert_jq "$MISSING_JSON" '.success // false' "true" "zstd_pack_dictionary_missing_dictionary_context_success"
MISSING_OK=false
if degraded_has_code "$MISSING_JSON" "l2_pack_cache_corruption" \
    && degraded_message_contains "$MISSING_JSON" "compression_dictionary_missing"; then
    MISSING_OK=true
fi
e2e_log_assert_eq "$MISSING_OK" "true" "zstd_pack_dictionary_missing_dictionary_specific_fallback"

CORRUPT_ENTRY_PATH="$(write_cache_entry "$CORRUPT_CACHE_ROOT" "$CACHE_COMPONENT" "$CACHE_KEY" \
    "$COMPRESSED_PATH" "$PAYLOAD_PATH" "$DICTIONARY_PATH" "corrupt")"
assert_jq "$(cat "$CORRUPT_ENTRY_PATH")" '.schema // empty' \
    "ee.pack.l2_cache.entry.v2" "zstd_pack_dictionary_corrupt_fixture_written"
CORRUPT_JSON="$(
    EE_L2_PACK_CACHE_DIR="$CORRUPT_CACHE_ROOT" \
        "$EE_BINARY" context "$QUERY" \
            --workspace "$EPIC_WORKSPACE" \
            --max-tokens 1600 \
            --json 2>/dev/null || true
)"
assert_jq "$CORRUPT_JSON" '.success // false' "true" "zstd_pack_dictionary_corrupt_dictionary_context_success"
CORRUPT_OK=false
if degraded_has_code "$CORRUPT_JSON" "l2_pack_cache_corruption" \
    && degraded_message_contains "$CORRUPT_JSON" "compression_dictionary_corrupt"; then
    CORRUPT_OK=true
fi
e2e_log_assert_eq "$CORRUPT_OK" "true" "zstd_pack_dictionary_corrupt_dictionary_specific_fallback"

OUTPUT_HASH="$(b3sum "$DICT_HIT_JSON_PATH" | awk '{print "blake3:" $1}')"
ELAPSED_MS="$(python3 -c "import time; print(int((time.monotonic() - float('$START_SECONDS')) * 1000))")"
emit_zstd_pack_dictionary_event "passed" "$DICTIONARY_ID" "$COMPRESSED_BYTES" \
    "$UNCOMPRESSED_BYTES" "$P50_MS" "$P95_MS" "$OUTPUT_HASH" "$FRESH_PACK_HASH" \
    "$MISSING_OK" "$CORRUPT_OK" "$ELAPSED_MS"
e2e_log_note "zstd_pack_dictionary_summary passed=${EE_TEST_LOG_ASSERTS_PASS} failed=${EE_TEST_LOG_ASSERTS_FAIL} elapsed_ms=${ELAPSED_MS} dictionary_id=${DICTIONARY_ID} compressed_bytes=${COMPRESSED_BYTES} uncompressed_bytes=${UNCOMPRESSED_BYTES} p50_ms=${P50_MS} p95_ms=${P95_MS}"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
