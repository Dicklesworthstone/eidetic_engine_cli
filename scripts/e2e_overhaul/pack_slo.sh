#!/usr/bin/env bash
# S4 — Resource-aware pack assembly SLO e2e driver.
#
# Exercises the public `ee context --resource-profile` surface with synthetic,
# deterministic corpora sized around each profile's candidate scan budget.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
if ! command -v python3 >/dev/null 2>&1; then
    echo "s4: python3 is required for deterministic pack slot contention" >&2
    exit 2
fi
epic_setup "epic_S_pack_slo"
ee_global profile config apply \
    --workspace "$EPIC_WORKSPACE" \
    --profile swarm \
    --json >/dev/null 2>&1 || true

seed_pack_slo_corpus() {
    local prefix="${1:?prefix required}"
    local count="${2:?count required}"
    local base="${3:?id base required}"
    local import_dir import_file i ordinal memory_id
    import_dir="$EPIC_WORKSPACE/s4-pack-slo-import-$prefix"
    import_file="$import_dir/memories.jsonl"
    mkdir -p "$import_dir"
    printf '{"schema":"ee.export.header.v1","format_version":1,"created_at":"2026-05-13T00:00:00Z","workspace_id":"ws_s4_pack_slo","workspace_path":"/s4/pack-slo","export_scope":"memories","redaction_level":"standard","record_count":%s,"ee_version":"s4-fixture","hostname":null,"export_id":"exp_s4_%s","import_source":"native","trust_level":"validated","checksum":null,"signature":null,"source_schema_version":null}\n' \
        "$count" "$prefix" > "$import_file"
    for i in $(seq 1 "$count"); do
        ordinal=$((base + i))
        memory_id="$(printf 'mem_%026d' "$ordinal")"
        printf '{"schema":"ee.export.memory.v1","memory_id":"%s","workspace_id":"ws_s4_pack_slo","level":"procedural","kind":"rule","content":"S4 %s resource profile memory %s: pack assembly must stay deterministic and bounded.","importance":0.8,"confidence":0.8,"utility":0.8,"created_at":"2026-05-13T00:00:01Z","updated_at":null,"tombstoned_at":null,"tombstoned_reason":null,"valid_from":null,"valid_to":null,"expires_at":null,"source_agent":"s4-pack-slo","provenance_uri":"ee-export://s4-pack-slo/%s/%s","superseded_by":null,"supersedes":null,"redacted":false,"redaction_reason":null}\n' \
            "$memory_id" "$prefix" "$i" "$prefix" "$i" >> "$import_file"
    done
    ee_workspace import jsonl --source "$import_file" --json >/dev/null 2>&1 || true
}

pack_slo_context() {
    local prefix="${1:?prefix required}"
    local profile="${2:?profile required}"
    local candidate_pool="${3:?candidate pool required}"
    ee_workspace context \
        "S4 $prefix resource profile deterministic bounded pack assembly" \
        --resource-profile "$profile" \
        --candidate-pool "$candidate_pool" \
        --json 2>/dev/null || true
}

hold_lean_pack_slot() {
    local slot_dir slot_path ready_path holder_pid waited
    slot_dir="$EPIC_WORKSPACE/.ee/pack-slots"
    slot_path="$slot_dir/lean-00.lock"
    ready_path="$slot_dir/lean-holder-ready-$$"
    mkdir -p "$slot_dir"

    python3 - "$slot_path" "$ready_path" >/dev/null 2>&1 <<'PY' &
import fcntl
import pathlib
import sys
import time

slot_path = sys.argv[1]
ready_path = pathlib.Path(sys.argv[2])
with open(slot_path, "a+", encoding="utf-8") as handle:
    fcntl.flock(handle, fcntl.LOCK_EX)
    ready_path.write_text("ready\n", encoding="utf-8")
    time.sleep(30)
PY
    holder_pid=$!

    for waited in $(seq 1 100); do
        if [ -f "$ready_path" ]; then
            printf '%s\n' "$holder_pid"
            return 0
        fi
        sleep 0.05
    done

    kill "$holder_pid" 2>/dev/null || true
    wait "$holder_pid" 2>/dev/null || true
    printf '%s\n' ""
    return 1
}

release_pack_slot_holder() {
    local holder_pid="${1:-}"
    if [ -n "$holder_pid" ]; then
        kill "$holder_pid" 2>/dev/null || true
        wait "$holder_pid" 2>/dev/null || true
    fi
}

assert_pack_slo() {
    local json="${1:?json required}"
    local profile="${2:?profile required}"
    local status="${3:?status required}"
    local label="${4:?label required}"
    assert_jq "$json" '.data.pack.slo.schema' "ee.pack.slo.v1" "${label}_schema"
    assert_jq "$json" '.data.pack.slo.profile' "$profile" "${label}_profile"
    assert_jq "$json" '.data.pack.slo.status' "$status" "${label}_status"
}

# Calibrated under-budget corpora for all three profiles.
seed_pack_slo_corpus "lean-within" 20 1000
LEAN_WITHIN_JSON=$(pack_slo_context "lean-within" lean 20)
assert_pack_slo "$LEAN_WITHIN_JSON" "lean" "within_budget" "s4_lean_within"

seed_pack_slo_corpus "standard-within" 40 2000
STANDARD_WITHIN_JSON=$(pack_slo_context "standard-within" standard 40)
assert_pack_slo "$STANDARD_WITHIN_JSON" "standard" "within_budget" "s4_standard_within"

seed_pack_slo_corpus "swarm-heavy-within" 60 3000
SWARM_WITHIN_JSON=$(pack_slo_context "swarm-heavy-within" swarm_heavy 60)
assert_pack_slo "$SWARM_WITHIN_JSON" "swarm_heavy" "within_budget" "s4_swarm_heavy_within"

# Deterministic warning/failure transitions around the lean candidate budget.
seed_pack_slo_corpus "lean-warning" 80 4000
LEAN_WARNING_JSON=$(pack_slo_context "lean-warning" lean 80)
assert_pack_slo "$LEAN_WARNING_JSON" "lean" "warning" "s4_lean_warning"
assert_jq "$LEAN_WARNING_JSON" \
    '[.data.pack.slo.degradations[]?.code] | index("pack_assembly_slow") != null' \
    "true" \
    "s4_lean_warning_code"

seed_pack_slo_corpus "lean-failure" 81 5000
LEAN_FAILURE_JSON=$(pack_slo_context "lean-failure" lean 81)
assert_pack_slo "$LEAN_FAILURE_JSON" "lean" "failure" "s4_lean_failure"
assert_jq "$LEAN_FAILURE_JSON" \
    '[.data.pack.slo.degradations[]?.code] | index("pack_assembly_budget_exceeded") != null' \
    "true" \
    "s4_lean_failure_code"

# SLO determinism: timing fields may vary, but status/categorization must not.
signature() {
    jq -c '{status: .data.pack.slo.status, codes: [.data.pack.slo.degradations[]?.code] | sort}' \
        2>/dev/null
}

SIG_1=$(printf '%s' "$(pack_slo_context "lean-warning" lean 80)" | signature)
SIG_2=$(printf '%s' "$(pack_slo_context "lean-warning" lean 80)" | signature)
SIG_3=$(printf '%s' "$(pack_slo_context "lean-warning" lean 80)" | signature)
e2e_log_assert_eq "$SIG_1" "$SIG_2" "s4_slo_determinism_run_1_2"
e2e_log_assert_eq "$SIG_2" "$SIG_3" "s4_slo_determinism_run_2_3"

# Deterministic slot contention: hold the lean profile's only pack slot with
# a separate process, then verify the public context surface emits the J6 code.
LEAN_SLOT_HOLDER_PID="$(hold_lean_pack_slot)"
if [ -n "$LEAN_SLOT_HOLDER_PID" ]; then
    LEAN_CONCURRENT_JSON=$(pack_slo_context "lean-within" lean 20)
    release_pack_slot_holder "$LEAN_SLOT_HOLDER_PID"
    assert_pack_slo "$LEAN_CONCURRENT_JSON" "lean" "warning" "s4_lean_concurrent_limit"
    assert_jq "$LEAN_CONCURRENT_JSON" \
        '[.data.pack.slo.degradations[]?.code] | index("pack_concurrent_limit_reached") != null' \
        "true" \
        "s4_lean_concurrent_limit_code"
    assert_jq "$LEAN_CONCURRENT_JSON" \
        '[.data.degraded[]?.code] | index("pack_concurrent_limit_reached") != null' \
        "true" \
        "s4_lean_concurrent_limit_context_code"
else
    e2e_log_assert_eq "pack_slot_holder_started" "pack_slot_holder_failed" "s4_lean_concurrent_holder"
fi
