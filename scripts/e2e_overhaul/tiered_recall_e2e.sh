#!/usr/bin/env bash
# Tiered hot/warm/cold recall e2e with cache prewarm and degraded-code fixtures.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

require_jq
if ! command -v python3 >/dev/null 2>&1; then
    echo "tiered_recall: python3 is required" >&2
    exit 2
fi

epic_setup "tiered_recall_e2e"

ARTIFACT_DIR="$EPIC_WORKSPACE/tiered-recall-artifacts"
IMPORT_DIR="$ARTIFACT_DIR/import"
IMPORT_JSONL="$IMPORT_DIR/memories.jsonl"
HOTSET_JSON="$ARTIFACT_DIR/hotset.json"
STALE_HOTSET_JSON="$ARTIFACT_DIR/stale_hotset.json"
SUMMARY_JSON="$ARTIFACT_DIR/summary.json"
METRICS_JSONL="$ARTIFACT_DIR/context_metrics.jsonl"
QUERY="tiered recall cold required sentinel cache prewarm outage"
CURRENT_GENERATION=44
TIER_GENERATION=44
TIERED_RECALL_FILLER_COUNT=650
TIERED_RECALL_MEMORY_COUNT=$((TIERED_RECALL_FILLER_COUNT + 1))
CONTEXT_CANDIDATE_POOL="${EE_TIERED_RECALL_CANDIDATE_POOL:-192}"
CONTEXT_MAX_TOKENS=20000

mkdir -p "$IMPORT_DIR"
: > "$METRICS_JSONL"

e2e_log_assert_num "$CONTEXT_CANDIDATE_POOL" -lt "$TIERED_RECALL_MEMORY_COUNT" \
    "tiered_recall_candidate_pool_bounded"
e2e_log_assert_num "$CONTEXT_CANDIDATE_POOL" -gt 128 \
    "tiered_recall_candidate_pool_exercises_warm_tier"

now_ns() {
    python3 -c 'from time import time_ns; print(time_ns())'
}

sha256_file() {
    shasum -a 256 "$1" | awk '{print $1}'
}

write_pack_config() {
    local enabled="${1:?enabled flag required}"
    mkdir -p "$EPIC_WORKSPACE/.ee"
    python3 - "$EPIC_WORKSPACE/.ee/config.toml" "$enabled" <<'PY'
import sys

path, enabled = sys.argv[1:]
with open(path, "w", encoding="utf-8") as handle:
    handle.write("[pack]\n")
    handle.write(f"memory_tier_admission = {enabled.lower()}\n")
PY
}

seed_tiered_corpus() {
    python3 - "$IMPORT_JSONL" "$EPIC_WORKSPACE" "$TIERED_RECALL_FILLER_COUNT" <<'PY'
import json
import sys

path, workspace, filler_count = sys.argv[1:]
filler_count = int(filler_count)
records = []
for index in range(filler_count):
    records.append(
        {
            "memory_id": f"mem_{121000 + index:026d}",
            "level": "procedural" if index % 3 == 0 else "semantic",
            "kind": "rule" if index % 3 == 0 else "fact",
            "content": (
                f"Tiered recall hot warm filler {index:03d}: tiered recall cache prewarm "
                "outage evidence keeps the candidate pool above the hot and warm budgets."
            ),
            "confidence": 0.95,
            "utility": 0.95,
            "importance": 0.95,
        }
    )
records.append(
    {
        "memory_id": "mem_00000000000000000000121651",
        "level": "episodic",
        "kind": "failure",
        "content": (
            "Tiered recall COLD REQUIRED SENTINEL cache prewarm outage evidence: "
            "cold required failure evidence must remain available and explainable."
        ),
        "confidence": 0.02,
        "utility": 0.02,
        "importance": 0.02,
    }
)
header = {
    "schema": "ee.export.header.v1",
    "format_version": 1,
    "created_at": "2026-05-25T00:00:00Z",
    "workspace_id": "ws_tiered_recall_e2e",
    "workspace_path": workspace,
    "export_scope": "memories",
    "redaction_level": "standard",
    "record_count": len(records),
    "ee_version": "tiered-recall-e2e",
    "hostname": None,
    "export_id": "exp_tiered_recall_e2e",
    "import_source": "native",
    "trust_level": "validated",
    "checksum": None,
    "signature": None,
    "source_schema_version": None,
}
footer = {
    "schema": "ee.export.footer.v1",
    "export_id": "exp_tiered_recall_e2e",
    "completed_at": "2026-05-25T00:00:02Z",
    "total_records": len(records) + 2,
    "memory_count": len(records),
    "link_count": 0,
    "tag_count": 0,
    "audit_count": 0,
    "checksum": None,
    "success": True,
    "error_message": None,
}
with open(path, "w", encoding="utf-8") as handle:
    handle.write(json.dumps(header, sort_keys=True) + "\n")
    for record in records:
        payload = {
            "schema": "ee.export.memory.v1",
            "workspace_id": "ws_tiered_recall_e2e",
            "created_at": "2026-05-25T00:00:01Z",
            "updated_at": None,
            "tombstoned_at": None,
            "tombstoned_reason": None,
            "valid_from": None,
            "valid_to": None,
            "expires_at": None,
            "source_agent": "tiered-recall-e2e",
            "provenance_uri": f"ee-export://tiered-recall/{record['memory_id']}",
            "superseded_by": None,
            "supersedes": None,
            "redacted": False,
            "redaction_reason": None,
            **record,
        }
        handle.write(json.dumps(payload, sort_keys=True) + "\n")
    handle.write(json.dumps(footer, sort_keys=True) + "\n")
PY
    ee_workspace import jsonl --source "$IMPORT_JSONL" --json > "$ARTIFACT_DIR/import.json"
    assert_jq "$(cat "$ARTIFACT_DIR/import.json")" '.success // false' "true" "tiered_recall_import_success"
    assert_jq "$(cat "$ARTIFACT_DIR/import.json")" \
        "(.data.memoriesImported // 0) >= $TIERED_RECALL_MEMORY_COUNT" \
        "true" "tiered_recall_imported_memory_count"
    ee_workspace index rebuild --json > "$ARTIFACT_DIR/index_rebuild.json"
    assert_jq "$(cat "$ARTIFACT_DIR/index_rebuild.json")" '.success // false' "true" "tiered_recall_index_rebuild_success"
    _e2e_emit_event "tiered_recall_seeded" \
        "memoryCount" "$TIERED_RECALL_MEMORY_COUNT" \
        "importJsonl" "$IMPORT_JSONL"
}

write_hotset_manifest() {
    local path="${1:?path required}"
    local generation="${2:?generation required}"
    python3 - "$path" "$generation" <<'PY'
import json
import sys

path = sys.argv[1]
generation = int(sys.argv[2])
manifest = {
    "schema": "ee.cache.hotset.v1",
    "workspaceId": "ws_tiered_recall_e2e",
    "workspaceGeneration": generation,
    "indexGeneration": generation,
    "admissionThreshold": 44,
    "profileTier": "standard",
    "redactionStatus": "content_not_stored",
    "candidateCount": 4,
    "admittedCount": 4 if generation == 44 else 0,
    "rejectedStaleCount": 0 if generation == 44 else 4,
    "memoryBudget": {
        "maxEntries": 32,
        "maxBytes": 1048576,
        "currentEntries": 4,
        "currentBytes": 1536,
    },
    "searchEntries": [
        {
            "key": "mem_00000000000000000000121000",
            "kind": "memory",
            "generation": generation,
            "estimatedBytes": 384,
            "hitCount": 8,
            "redactionStatus": "content_not_stored",
        },
        {
            "key": "mem_00000000000000000000121450",
            "kind": "memory",
            "generation": generation,
            "estimatedBytes": 384,
            "hitCount": 5,
            "redactionStatus": "content_not_stored",
        },
        {
            "key": "mem_00000000000000000000121651",
            "kind": "memory",
            "generation": generation,
            "estimatedBytes": 384,
            "hitCount": 2,
            "redactionStatus": "content_not_stored",
        },
    ],
    "packEntries": [
        {
            "key": "pack:tiered-recall:evidence",
            "kind": "pack_section",
            "section": "evidence",
            "generation": generation,
            "estimatedBytes": 384,
            "hitCount": 4,
            "redactionStatus": "content_not_stored",
        }
    ],
    "rejectedStaleSearchEntries": [],
    "rejectedStalePackEntries": [],
    "degraded": [],
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(manifest, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
}

normalize_hash() {
    local path="${1:?path required}"
    python3 - "$path" <<'PY' | shasum -a 256 | awk '{print $1}'
import json
import sys

volatile = {
    "generatedAt",
    "generated_at",
    "runDurationMs",
    "elapsedMs",
    "elapsedMsBucket",
    "nondeterministic",
    "observedAt",
    "leaseHeldMs",
}

def scrub(value):
    if isinstance(value, dict):
        return {key: scrub(val) for key, val in sorted(value.items()) if key not in volatile}
    if isinstance(value, list):
        return [scrub(item) for item in value]
    return value

with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
print(json.dumps(scrub(payload), sort_keys=True, separators=(",", ":")))
PY
}

run_context_probe() {
    local label="${1:?label required}"
    local out_file="$ARTIFACT_DIR/${label}.json"
    local err_file="$ARTIFACT_DIR/${label}.stderr"
    local start_ns end_ns elapsed_ms json
    start_ns="$(now_ns)"
    "$EE_BINARY" context "$QUERY" \
        --workspace "$EPIC_WORKSPACE" \
        --candidate-pool "$CONTEXT_CANDIDATE_POOL" \
        --max-tokens "$CONTEXT_MAX_TOKENS" \
        --json >"$out_file" 2>"$err_file"
    end_ns="$(now_ns)"
    elapsed_ms=$(( (end_ns - start_ns) / 1000000 ))
    json="$(cat "$out_file")"
    assert_jq "$json" '.success // false' "true" "tiered_recall_${label}_context_success"
    python3 - "$label" "$elapsed_ms" "$out_file" "$err_file" "$METRICS_JSONL" <<'PY'
import hashlib
import json
import sys

label, elapsed_ms, out_file, err_file, metrics_path = sys.argv[1:]
with open(out_file, "rb") as handle:
    stdout = handle.read()
with open(err_file, "rb") as handle:
    stderr = handle.read()
payload = json.loads(stdout)
items = (((payload.get("data") or {}).get("pack") or {}).get("items") or [])
why_text = "\n".join(str(item.get("why") or "") for item in items)
codes = [
    entry.get("code")
    for entry in (payload.get("degraded") or []) + ((payload.get("data") or {}).get("degraded") or [])
    if isinstance(entry, dict) and entry.get("code")
]
record = {
    "schema": "ee.test_event.v1",
    "phase": label,
    "elapsedMs": int(elapsed_ms),
    "stdoutHash": "sha256:" + hashlib.sha256(stdout).hexdigest(),
    "stderrHash": "sha256:" + hashlib.sha256(stderr).hexdigest(),
    "itemCount": len(items),
    "tierAdmissionCount": why_text.count("tierAdmission"),
    "hotRecallCount": why_text.count("tier=hot"),
    "warmRecallCount": why_text.count("tier=warm"),
    "coldRecallCount": why_text.count("tier=cold"),
    "requiredColdRecallCount": why_text.count("requiredEvidencePreserved=true"),
    "degradedCodes": codes,
}
with open(metrics_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(record, sort_keys=True) + "\n")
PY
    printf '%s\n' "$out_file"
}

latency_percentile() {
    local percentile="${1:?percentile required}"
    python3 - "$METRICS_JSONL" "$percentile" <<'PY'
import json
import math
import sys

path, percentile = sys.argv[1:]
values = []
with open(path, encoding="utf-8") as handle:
    for line in handle:
        if line.strip():
            values.append(json.loads(line)["elapsedMs"])
values.sort()
if not values:
    print(0)
else:
    index = max(0, min(len(values) - 1, math.ceil((float(percentile) / 100.0) * len(values)) - 1))
    print(values[index])
PY
}

assert_fixture() {
    local code="${1:?code required}"
    local path="$REPO_ROOT/tests/fixtures/failure_modes/${code}.json"
    jq empty "$path"
    e2e_log_assert_eq "$(jq -r '.code' "$path")" "$code" "tiered_recall_fixture_${code}"
}

seed_tiered_corpus
write_hotset_manifest "$HOTSET_JSON" "$CURRENT_GENERATION"
write_hotset_manifest "$STALE_HOTSET_JSON" 40

write_pack_config false
DISABLED_FIRST="$(run_context_probe disabled_first)"
DISABLED_SECOND="$(run_context_probe disabled_second)"
DISABLED_HASH_FIRST="$(normalize_hash "$DISABLED_FIRST")"
DISABLED_HASH_SECOND="$(normalize_hash "$DISABLED_SECOND")"
e2e_log_assert_eq "$DISABLED_HASH_FIRST" "$DISABLED_HASH_SECOND" "tiered_recall_disabled_hash_parity"
if grep -q "tierAdmission" "$DISABLED_FIRST" "$DISABLED_SECOND"; then
    e2e_log_assert_eq "tierAdmission present" "tierAdmission absent" "tiered_recall_disabled_no_tier_admission"
else
    e2e_log_assert_eq "tierAdmission absent" "tierAdmission absent" "tiered_recall_disabled_no_tier_admission"
fi

write_pack_config true
ENABLED_FIRST="$(run_context_probe enabled_first)"
ENABLED_SECOND="$(run_context_probe enabled_second)"
ENABLED_HASH_FIRST="$(normalize_hash "$ENABLED_FIRST")"
ENABLED_HASH_SECOND="$(normalize_hash "$ENABLED_SECOND")"
e2e_log_assert_eq "$ENABLED_HASH_FIRST" "$ENABLED_HASH_SECOND" "tiered_recall_enabled_hash_parity"
assert_jq "$(cat "$ENABLED_FIRST")" \
    '[.data.pack.items[]? | select((.why // "") | contains("tierAdmission tier=hot"))] | length > 0' \
    "true" "tiered_recall_hot_explained"
assert_jq "$(cat "$ENABLED_FIRST")" \
    '[.data.pack.items[]? | select((.why // "") | contains("tierAdmission tier=cold") and contains("requiredEvidencePreserved=true") and contains("noFilter=true"))] | length > 0' \
    "true" "tiered_recall_required_cold_explained"

PREWARM_JSON="$(ee_workspace cache prewarm \
    --from-hotset "$HOTSET_JSON" \
    --profile standard \
    --current-generation "$CURRENT_GENERATION" \
    --json 2>"$ARTIFACT_DIR/prewarm.stderr")"
printf '%s\n' "$PREWARM_JSON" > "$ARTIFACT_DIR/prewarm.json"
assert_jq "$PREWARM_JSON" '.success // false' "true" "tiered_recall_cache_prewarm_success"
assert_jq "$PREWARM_JSON" '(.data.admitted.totalEntries // 0) >= 3' "true" "tiered_recall_cache_hotset_admitted"

STALE_JSON="$(ee_workspace cache prewarm \
    --from-hotset "$STALE_HOTSET_JSON" \
    --profile standard \
    --current-generation "$CURRENT_GENERATION" \
    --json 2>"$ARTIFACT_DIR/stale_prewarm.stderr")"
printf '%s\n' "$STALE_JSON" > "$ARTIFACT_DIR/stale_prewarm.json"
assert_jq "$STALE_JSON" '.success // false' "true" "tiered_recall_stale_hotset_success"
assert_jq "$STALE_JSON" '([.data.degraded[]?.code] | index("cache_hotset_stale") != null)' \
    "true" "tiered_recall_stale_hotset_degraded_code"

assert_fixture "memory_tier_metadata_stale"
assert_fixture "cache_hotset_stale"
assert_fixture "hotset_prewarm_no_signals"

P50_MS="$(latency_percentile 50)"
P95_MS="$(latency_percentile 95)"
python3 - "$SUMMARY_JSON" "$METRICS_JSONL" "$DISABLED_HASH_FIRST" "$ENABLED_HASH_FIRST" \
    "$P50_MS" "$P95_MS" "$TIER_GENERATION" "$CURRENT_GENERATION" \
    "$CONTEXT_CANDIDATE_POOL" "$TIERED_RECALL_MEMORY_COUNT" <<'PY'
import json
import sys

(
    summary_path,
    metrics_path,
    disabled_hash,
    enabled_hash,
    p50_ms,
    p95_ms,
    tier_generation,
    current_generation,
    candidate_pool,
    corpus_memory_count,
) = sys.argv[1:]
metrics = []
with open(metrics_path, encoding="utf-8") as handle:
    for line in handle:
        if line.strip():
            metrics.append(json.loads(line))
summary = {
    "schema": "ee.e2e.tiered_recall.v1",
    "tierGeneration": int(tier_generation),
    "currentGeneration": int(current_generation),
    "candidatePool": int(candidate_pool),
    "corpusMemoryCount": int(corpus_memory_count),
    "latencyMs": {"p50": int(p50_ms), "p95": int(p95_ms)},
    "hashParity": {
        "disabledScrubbedHash": "sha256:" + disabled_hash,
        "enabledScrubbedHash": "sha256:" + enabled_hash,
    },
    "tierCounts": {
        "tierAdmission": sum(record["tierAdmissionCount"] for record in metrics if record["phase"].startswith("enabled")),
        "hot": sum(record["hotRecallCount"] for record in metrics if record["phase"].startswith("enabled")),
        "warm": sum(record["warmRecallCount"] for record in metrics if record["phase"].startswith("enabled")),
        "coldRecall": sum(record["coldRecallCount"] for record in metrics),
        "requiredColdRecall": sum(record["requiredColdRecallCount"] for record in metrics),
    },
    "cachePrewarmEffect": {
        "hotsetManifest": "content_not_stored",
        "staleHotsetDegradedCode": "cache_hotset_stale",
    },
}
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY

assert_jq "$(cat "$SUMMARY_JSON")" '.schema' "ee.e2e.tiered_recall.v1" "tiered_recall_summary_schema"
assert_jq "$(cat "$SUMMARY_JSON")" '(.candidatePool < .corpusMemoryCount)' "true" \
    "tiered_recall_summary_bounded_candidate_pool"
assert_jq "$(cat "$SUMMARY_JSON")" '(.latencyMs.p95 >= .latencyMs.p50)' "true" "tiered_recall_latency_order"
assert_jq "$(cat "$SUMMARY_JSON")" '(.tierCounts.requiredColdRecall > 0)' "true" "tiered_recall_summary_cold_recall"
_e2e_emit_event "tiered_recall_summary" \
    "summary_json" "$SUMMARY_JSON" \
    "tierCounts" "$(jq -c '.tierCounts' "$SUMMARY_JSON")" \
    "tierGeneration" "$TIER_GENERATION" \
    "currentGeneration" "$CURRENT_GENERATION" \
    "p50ContextLatencyMs" "$P50_MS" \
    "p95ContextLatencyMs" "$P95_MS" \
    "cachePrewarmEffect" "$(jq -c '.cachePrewarmEffect' "$SUMMARY_JSON")" \
    "disabledHash" "sha256:$DISABLED_HASH_FIRST" \
    "enabledHash" "sha256:$ENABLED_HASH_FIRST"

if [ "${EE_TEST_LOG_ASSERTS_FAIL:-0}" -gt 0 ]; then
    exit 1
fi
