#!/usr/bin/env bash
# J7 — Epic J determinism harness.
#
# AGENTS.md non-negotiable: given the same DB + indexes + config + query,
# every machine-facing JSON output must be byte-stable across runs and the
# context pack hash must reproduce exactly. This driver spawns `ee` as a
# child process three times per surface, strips known time-varying fields
# from each response, and asserts the resulting canonical JSON hashes
# blake3-equal across all three runs.
#
# Surfaces exercised:
#   - ee search "<q>" --json
#   - ee context "<q>" --max-tokens N --json
#   - ee memory list --json
#   - ee status --json
#   - ee doctor --json
#   - ee why <id> --json
#   - ee export --output-dir <dir> --json
#
# Tie-break check:
#   Two memories whose content is byte-identical produce equal scores.
#   The harness seeds such a pair and asserts the resulting result order
#   is stable across all three runs (memory_id ascending — the documented
#   secondary sort).
#
# Run-process isolation:
#   Each invocation is a separate child process so state leaks (mtime,
#   PID, in-process caches, RNG seeds) surface here even though they
#   would not surface inside a single-process unit test.
#
# Bead: bd-17c65.10.7 (J7).

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"

DUELING_WIZARDS_DETERMINISM_MANIFEST="$REPO_ROOT/tests/fixtures/contracts/dueling_wizards_determinism_gate.json"

# Resolve a content hasher: prefer blake3sum (matches ee's pack hash),
# fall back to shasum -a 256 on systems where blake3 isn't installed.
# Either is fine for byte-equality checking; only the absolute value
# would differ across machines, never the equality result.
hash_stdin() {
    if command -v blake3sum >/dev/null 2>&1; then
        blake3sum | awk '{print $1}'
    else
        shasum -a 256 | awk '{print $1}'
    fi
}

# Mirror of ee::obs::VOLATILE_FIELD_NAMES — tests/volatile_field_registry_consistency_test.rs
# pins this list to the Rust registry entry-for-entry, in order.
VOLATILE_FIELD_NAMES=(
    generatedAt
    generated_at
    createdAt
    created_at
    updatedAt
    completedAt
    finishedAt
    expiresAt
    capturedAt
    captured_at
    computedAt
    computed_at
    observedAt
    recordedAt
    refreshedAt
    selectedAt
    decidedAt
    estimatedAt
    exposedAt
    lastValidatedAt
    last_accessed
    last_accessed_at
    last_seen_at
    last_used_at
    audit_ts
    elapsedMs
    elapsed_ms
    elapsedMsBucket
    durationMs
    wallClockMs
    startedAt
    started_at
    endedAt
    ended_at
    ts
    timestamp
    runIndex
    run_index
    runDurationMs
    run_duration_ms
    ee_binary_hash
    capsule_id
    integrity
    swarm_brief_summary
    swarm_incident_summary
    swarm_replay_summary
    environment_attestation_summary
    pack_replay_summary
    proof_broker_summary
    regression_causality_summary
    shadow_policy_summary
    contention_summary
    databasePath
    workspacePath
    indexDir
    snapshotRefreshedAt
    witnessElapsedMs
    witnessRecordedAt
    algorithmStartedAt
    projectionMs
    pagerankMs
    betweennessMs
    totalMs
    selfNodeKey
    selfTailscaleIp
    selfMagicDnsName
    tailnetId
    tailnetDisplayName
    selfAdvertisedTags
    peerNodeKey
    peerTailscaleIps
    peerMagicDnsName
    peerHostname
    peerAdvertisedTags
    binaryVersionRaw
    binaryAbsolutePath
)

volatile_field_delete_filter() {
    local filter='walk(if type == "object" then del('
    local separator=""
    local field

    for field in "${VOLATILE_FIELD_NAMES[@]}"; do
        filter="${filter}${separator}.${field}"
        separator=","
    done

    printf '%s) else . end)\n' "$filter"
}

# Strip every JSON field whose value legitimately varies per invocation
# (timestamps, wall-clock elapsed counters, runtime-allocated IDs that
# carry no semantic load). The list is the union of the variable-field
# inventory across the surfaces exercised here; deleting a key that
# doesn't exist on a given response is a no-op so the same filter
# applies uniformly.
#
# Why `walk(...)`: time-varying fields appear at multiple nesting
# depths (e.g. `data.metrics.elapsedMs` AND `data.results[].why` —
# the latter shouldn't be stripped, only known-variable fields).
strip_variable_fields() {
    jq "$(volatile_field_delete_filter)"
}

assert_dueling_wizards_determinism_manifest() {
    local manifest="$DUELING_WIZARDS_DETERMINISM_MANIFEST"
    local summary surface_count pack_hash_count

    if [ ! -f "$manifest" ]; then
        e2e_log_assert_eq "missing" "present" "determinism_dueling_wizards_manifest_exists"
        return 1
    fi

    if ! summary=$(jq -er '
        ["why_not", "harvest", "calibration", "impact", "error_recall", "blind_spots", "conflict", "read_fence_consistency", "pack_lod", "feedback_roi"] as $surfaces
        | ["byte_identical_json", "volatile_fields_explicit", "stable_ordering", "stderr_or_artifact_diagnostics"] as $required_assertions
        | if .schema != "ee.dueling_wizards.determinism_gate.v1" then
            error("schema must be ee.dueling_wizards.determinism_gate.v1")
          elif .gateBead != "bd-1n0np.15.2" then
            error("gateBead must be bd-1n0np.15.2")
          elif .implementationState != "planned_contract" then
            error("implementationState must remain planned_contract until runtime rows are wired")
          elif .policy.runCount != 3 then
            error("policy.runCount must be 3")
          elif .policy.canonicalization != "explicit_volatile_field_removal" then
            error("policy.canonicalization drifted")
          elif .policy.stdoutMachineOnly != true then
            error("policy.stdoutMachineOnly must be true")
          elif .policy.localCargoProof != "invalid" then
            error("policy.localCargoProof must be invalid")
          elif .policy.rchProofRequiredForRuntimeTests != true then
            error("policy.rchProofRequiredForRuntimeTests must be true")
          elif (([.determinismMatrix[].surface] | sort) != ($surfaces | sort)) then
            error("determinismMatrix surface set drifted")
          elif (([.surfaceCoverageMatrix[].surface] | sort) != ($surfaces | sort)) then
            error("surfaceCoverageMatrix surface set drifted")
          elif (.determinismMatrix | length) != 10 then
            error("determinismMatrix must carry 10 dueling-wizards surfaces")
          elif (.surfaceCoverageMatrix | length) != 10 then
            error("surfaceCoverageMatrix must carry 10 dueling-wizards surfaces")
          elif (all(.determinismMatrix[];
            .runCount == 3
            and .canonicalization == "explicit_volatile_field_removal"
            and .stdoutMachineOnly == true
            and .diagnosticsChannel == "stderr_or_artifact"
            and .runtimeProof == "rch_only"
            and ((.requiredAssertions | sort) == ($required_assertions | sort))
            and (if .packHashExpected then
              .packHashAbsenceFailure == true and .packHashField == "data.pack.hash"
            else
              .packHashAbsenceFailure == false and .packHashField == null
            end)
          ) | not) then
            error("determinismMatrix policy row drifted")
          elif (all(.surfaceCoverageMatrix[];
            .mustClauses == 9
            and .tested == 9
            and .passing == 9
            and .divergent == 0
            and .scoreMilli >= 950
            and .determinismStatus == "three_run_contract_declared"
            and .runtimeProofPolicy == "rch_required_local_invalid"
            and .complianceStatus == "declared_conformant"
          ) | not) then
            error("surfaceCoverageMatrix coverage row drifted")
          else
            [(.determinismMatrix | length), ([.determinismMatrix[] | select(.packHashExpected)] | length)] | @tsv
          end
    ' "$manifest"); then
        e2e_log_assert_eq "invalid" "valid" "determinism_dueling_wizards_manifest_contract"
        return 1
    fi

    IFS=$'\t' read -r surface_count pack_hash_count <<< "$summary"
    e2e_log_assert_eq "true" "true" "determinism_dueling_wizards_manifest_contract"
    e2e_log_note "dueling_wizards_determinism_manifest surfaces=$surface_count pack_hash_rows=$pack_hash_count"
}

if [ "${BASH_SOURCE[0]}" != "$0" ]; then
    return 0
fi

require_jq
epic_setup "epic_J_determinism"
assert_dueling_wizards_determinism_manifest

# Run `ee ARGS...` three times, canonicalize each output, hash, and
# emit an assert via the J1 logger. The assert name is the first arg.
run_3x_assert_identical() {
    local name="$1"
    shift
    local run output command_status canonical_hash stderr_path
    local -a hashes=()
    for run in 1 2 3; do
        stderr_path="${EPIC_WORKSPACE}/determinism_${name}_run_${run}.stderr"
        output=$("$EE_BINARY" "$@" --workspace "$EPIC_WORKSPACE" 2>"$stderr_path")
        command_status=$?
        if [ "$command_status" -ne 0 ] \
            || ! printf '%s' "$output" | jq -e \
                '.schema == "ee.response.v2" and .success == true' >/dev/null 2>&1; then
            e2e_log_assert_eq "run=$run exit=$command_status" \
                "exit=0 ee.response.v2 success=true" \
                "determinism_${name}_run_valid"
            return 1
        fi
        canonical_hash=$(printf '%s' "$output" \
            | strip_variable_fields | jq -S '.' | hash_stdin)
        command_status=$?
        if [ "$command_status" -ne 0 ] || [ -z "$canonical_hash" ]; then
            e2e_log_assert_eq \
                "run=$run canonicalize_exit=$command_status hash=${canonical_hash:-<empty>}" \
                "canonicalize_exit=0 nonempty_hash" \
                "determinism_${name}_canonicalize_valid"
            return 1
        fi
        hashes+=("$canonical_hash")
    done
    if [ "${hashes[0]}" = "${hashes[1]}" ] && [ "${hashes[1]}" = "${hashes[2]}" ]; then
        e2e_log_assert_eq "true" "true" "determinism_${name}"
        e2e_log_note "determinism_${name}_hash=${hashes[0]}"
    else
        e2e_log_assert_eq \
            "h1=${hashes[0]} h2=${hashes[1]} h3=${hashes[2]}" \
            "all_equal" "determinism_${name}"
        e2e_log_note \
            "determinism_${name}_diverged h1=${hashes[0]} h2=${hashes[1]} h3=${hashes[2]}"
        return 1
    fi
    return 0
}

# ---------------------------------------------------------------------------
# Seed the workspace with the 2026-05-10 reference corpus + a deterministic
# tie pair (two memories that hash-embed identically because the only
# difference is the leading punctuation).
# ---------------------------------------------------------------------------
seed_corpus
TIE_A=$("$EE_BINARY" remember "Run cargo fmt before release v0.1.0." \
    --workspace "$EPIC_WORKSPACE" --level procedural --kind rule --json \
    | jq -r '.data.memory.id // .data.memoryId // .data.id // empty')
TIE_B=$("$EE_BINARY" remember "Run cargo fmt before release v0.2.0." \
    --workspace "$EPIC_WORKSPACE" --level procedural --kind rule --json \
    | jq -r '.data.memory.id // .data.memoryId // .data.id // empty')
e2e_log_note "tie_pair tie_a=${TIE_A:-?} tie_b=${TIE_B:-?}"

# Pick a memory id to drive ee why. Use the first listed memory.
ANY_MEM=$("$EE_BINARY" memory list --workspace "$EPIC_WORKSPACE" --json 2>/dev/null \
    | jq -r '(.data.memories // .data.items // []) | .[0].id // .[0].memoryId // empty')
e2e_log_note "why_target=${ANY_MEM:-?}"

# ---------------------------------------------------------------------------
# Surface 1 — ee search.
# Three runs against the same query must hash-equal after canonicalization.
# ---------------------------------------------------------------------------
run_3x_assert_identical "search_json" search "cargo fmt release" --json

# Stronger check: --explain on, --relevance-floor pinned, results['why']
# and explanation['factors'] must be byte-stable.
run_3x_assert_identical \
    "search_json_explain_pinned_floor" \
    search "cargo fmt release" --json --explain --relevance-floor 0.0

# ---------------------------------------------------------------------------
# Surface 2 — ee context. The pack.hash must reproduce exactly because the
# pack-hash input is a documented invariant (see AGENTS.md determinism
# rules + commit 8f6c011 BTreeMap reproducibility).
# ---------------------------------------------------------------------------
run_3x_assert_identical \
    "context_pack_json" \
    context "prepare release v0.2.0" --max-tokens 1000 --json

# Direct pack-hash inspection: extract just data.pack.hash from three runs
# and assert it is a single value with no variation.
PACK_HASH_1=$("$EE_BINARY" context "prepare release v0.2.0" \
    --workspace "$EPIC_WORKSPACE" --max-tokens 1000 --json 2>/dev/null \
    | jq -r '.data.pack.hash // empty')
PACK_HASH_2=$("$EE_BINARY" context "prepare release v0.2.0" \
    --workspace "$EPIC_WORKSPACE" --max-tokens 1000 --json 2>/dev/null \
    | jq -r '.data.pack.hash // empty')
PACK_HASH_3=$("$EE_BINARY" context "prepare release v0.2.0" \
    --workspace "$EPIC_WORKSPACE" --max-tokens 1000 --json 2>/dev/null \
    | jq -r '.data.pack.hash // empty')
if [ -n "$PACK_HASH_1" ] && [ "$PACK_HASH_1" = "$PACK_HASH_2" ] && [ "$PACK_HASH_2" = "$PACK_HASH_3" ]; then
    e2e_log_assert_eq "true" "true" "determinism_pack_hash_reproducible"
    e2e_log_note "pack_hash=$PACK_HASH_1"
else
    e2e_log_assert_eq "[$PACK_HASH_1 $PACK_HASH_2 $PACK_HASH_3]" "all_equal" "determinism_pack_hash_reproducible"
fi

# ---------------------------------------------------------------------------
# Surface 3 — ee memory list. Must be deterministic order across runs
# regardless of insertion timing.
# ---------------------------------------------------------------------------
run_3x_assert_identical "memory_list_json" memory list --json

# ---------------------------------------------------------------------------
# Surface 4 — ee status / doctor. After stripping timestamps, the posture
# block must be byte-stable.
# ---------------------------------------------------------------------------
run_3x_assert_identical "status_json" status --json
run_3x_assert_identical "doctor_json" doctor --json

# ---------------------------------------------------------------------------
# Surface 5 — ee why. Run against a known memory.
# ---------------------------------------------------------------------------
if [ -n "${ANY_MEM:-}" ]; then
    WHY_1=$("$EE_BINARY" why "$ANY_MEM" --workspace "$EPIC_WORKSPACE" --json 2>/dev/null \
        | strip_variable_fields | jq -S '.' | hash_stdin)
    WHY_2=$("$EE_BINARY" why "$ANY_MEM" --workspace "$EPIC_WORKSPACE" --json 2>/dev/null \
        | strip_variable_fields | jq -S '.' | hash_stdin)
    WHY_3=$("$EE_BINARY" why "$ANY_MEM" --workspace "$EPIC_WORKSPACE" --json 2>/dev/null \
        | strip_variable_fields | jq -S '.' | hash_stdin)
    if [ "$WHY_1" = "$WHY_2" ] && [ "$WHY_2" = "$WHY_3" ]; then
        e2e_log_assert_eq "true" "true" "determinism_why_json"
        e2e_log_note "why_hash=$WHY_1"
    else
        e2e_log_assert_eq "[$WHY_1 $WHY_2 $WHY_3]" "all_equal" "determinism_why_json"
    fi
else
    e2e_log_note "why_skipped_no_memory_in_workspace"
fi

# ---------------------------------------------------------------------------
# Surface 6 — ee export. Each run writes to a distinct directory; the
# manifestHash + recordsHash must match across runs because they hash
# the underlying durable content (not the wall-clock export ts).
# ---------------------------------------------------------------------------
EXPORT_HASHES=""
for run in 1 2 3; do
    out="$EPIC_WORKSPACE/export_run_$run"
    mkdir -p "$out"
    H=$("$EE_BINARY" export --workspace "$EPIC_WORKSPACE" --output-dir "$out" --json 2>/dev/null \
        | jq -r '.data.manifestHash // .data.manifest_hash // empty')
    EXPORT_HASHES="$EXPORT_HASHES $H"
done
IFS=' ' read -r EH1 EH2 EH3 <<< "$(echo "$EXPORT_HASHES" | xargs)"
if [ -n "${EH1:-}" ] && [ "$EH1" = "${EH2:-}" ] && [ "${EH2:-}" = "${EH3:-}" ]; then
    e2e_log_assert_eq "true" "true" "determinism_export_manifest_hash"
    e2e_log_note "export_manifest_hash=$EH1"
else
    e2e_log_note "export_manifest_skipped_or_diverged [$EH1, $EH2, $EH3]"
fi

# ---------------------------------------------------------------------------
# Tie-break: two memories with identical-shape content produce equal scores
# under hash-embedder + lexical. Order across three runs must be stable
# (memory_id ascending, the documented secondary sort).
# ---------------------------------------------------------------------------
TIE_ORDER_1=$("$EE_BINARY" search "cargo fmt before release" \
    --workspace "$EPIC_WORKSPACE" --limit 10 --relevance-floor 0 --json 2>/dev/null \
    | jq -r '[.data.results[].docId] | join(",")')
TIE_ORDER_2=$("$EE_BINARY" search "cargo fmt before release" \
    --workspace "$EPIC_WORKSPACE" --limit 10 --relevance-floor 0 --json 2>/dev/null \
    | jq -r '[.data.results[].docId] | join(",")')
TIE_ORDER_3=$("$EE_BINARY" search "cargo fmt before release" \
    --workspace "$EPIC_WORKSPACE" --limit 10 --relevance-floor 0 --json 2>/dev/null \
    | jq -r '[.data.results[].docId] | join(",")')
if [ -n "$TIE_ORDER_1" ] && [ "$TIE_ORDER_1" = "$TIE_ORDER_2" ] && [ "$TIE_ORDER_2" = "$TIE_ORDER_3" ]; then
    e2e_log_assert_eq "true" "true" "determinism_tie_break_order_stable"
    e2e_log_note "tie_order=$TIE_ORDER_1"
else
    e2e_log_assert_eq "[$TIE_ORDER_1 | $TIE_ORDER_2 | $TIE_ORDER_3]" "all_equal" "determinism_tie_break_order_stable"
fi

# Tie-break direction: when scores are identical, the lower memory_id
# must rank first (ULIDs sort lexicographically with time-prefix). If
# tie_a/tie_b are both present, assert tie_a (created first) sorts before
# tie_b in any pairwise occurrence — the test passes whether or not both
# pass the relevance floor, as long as we observe at least one ordered
# pair.
if [ -n "${TIE_A:-}" ] && [ -n "${TIE_B:-}" ]; then
    BOTH_PRESENT=$(printf '%s' "$TIE_ORDER_1" | tr ',' '\n' | grep -cE "^($TIE_A|$TIE_B)$" || true)
    if [ "$BOTH_PRESENT" -eq 2 ]; then
        POS_A=$(printf '%s' "$TIE_ORDER_1" | tr ',' '\n' | grep -nE "^$TIE_A$" | head -1 | cut -d: -f1)
        POS_B=$(printf '%s' "$TIE_ORDER_1" | tr ',' '\n' | grep -nE "^$TIE_B$" | head -1 | cut -d: -f1)
        if [ "$POS_A" -lt "$POS_B" ]; then
            e2e_log_assert_eq "true" "true" "determinism_tie_break_memory_id_ascending"
        else
            e2e_log_assert_eq "pos_a=$POS_A pos_b=$POS_B" "pos_a<pos_b" "determinism_tie_break_memory_id_ascending"
        fi
    else
        e2e_log_note "tie_pair_not_both_returned both_present=$BOTH_PRESENT"
    fi
fi

# ---------------------------------------------------------------------------
# Native reranker determinism (bd-1nl13.13).
#
# The model artifact is deliberately external to the source tree. A normal
# model-free lane records an honest skip; setting EE_E2E_RERANK_REQUIRE_MODEL=1
# makes every missing model input or degraded/fusion-only result fail closed.
# Five-target lanes additionally set EE_E2E_RERANK_REQUIRE_REFERENCE=1 and pass
# a reference vector: full content order is exact and calibrated scores must
# stay within the configured cross-platform tolerance.
# ---------------------------------------------------------------------------
RERANK_QUERY="bd1nl13 release format checklist cargo clippy"
RERANK_ORIGINAL_HOME="${HOME}"
RERANK_REQUIRE_MODEL="${EE_E2E_RERANK_REQUIRE_MODEL:-0}"
RERANK_REQUIRE_REFERENCE="${EE_E2E_RERANK_REQUIRE_REFERENCE:-0}"
RERANK_MODEL_ARCHIVE="${EE_E2E_RERANK_MODEL_ARCHIVE:-${RERANK_ORIGINAL_HOME}/.local/share/ee/models/rerank/rerank-default-v1/rerank-default-v1.tar.zst}"
RERANK_REFERENCE_VECTOR="${EE_E2E_RERANK_REFERENCE_VECTOR:-}"
RERANK_VECTOR_OUT="${EE_E2E_RERANK_VECTOR_OUT:-${EPIC_WORKSPACE}/rerank_determinism_vector.json}"
# Cross-platform SIMD/libm paths need a numerical tolerance, not bit identity.
# 0.01 is tight on the public sigmoid score while leaving the stronger exact
# ordering contract to catch behaviorally meaningful drift.
RERANK_SCORE_TOLERANCE="${EE_E2E_RERANK_SCORE_TOLERANCE:-0.01}"
RERANK_JSON_OUTPUT=""

if [ -n "$RERANK_REFERENCE_VECTOR" ]; then
    RERANK_REQUIRE_MODEL=1
fi
if [ "$RERANK_REQUIRE_REFERENCE" = "1" ]; then
    RERANK_REQUIRE_MODEL=1
fi

run_rerank_json() {
    local label="${1:?label required}"
    shift
    local command_status stderr_path
    stderr_path="${EPIC_WORKSPACE}/rerank_${label}.stderr"
    RERANK_JSON_OUTPUT=$("$EE_BINARY" "$@" --workspace "$EPIC_WORKSPACE" 2>"$stderr_path")
    command_status=$?
    if [ "$command_status" -ne 0 ] \
        || ! printf '%s' "$RERANK_JSON_OUTPUT" | jq -e \
            '.schema == "ee.response.v2" and .success == true' >/dev/null 2>&1; then
        e2e_log_assert_eq \
            "exit=$command_status schema=$(printf '%s' "$RERANK_JSON_OUTPUT" | jq -r '.schema // "<missing>"' 2>/dev/null) success=$(printf '%s' "$RERANK_JSON_OUTPUT" | jq -r '.success // "<missing>"' 2>/dev/null)" \
            "exit=0 schema=ee.response.v2 success=true" \
            "rerank_${label}_valid"
        return 1
    fi
    e2e_log_assert_eq "true" "true" "rerank_${label}_valid"
    return 0
}

remember_rerank_fixture() {
    local label="${1:?label required}"
    local level="${2:?level required}"
    local kind="${3:?kind required}"
    local content="${4:?content required}"
    run_rerank_json "$label" remember "$content" \
        --level "$level" --kind "$kind" \
        --no-auto-link --no-propose-candidates --json
}

rerank_input_failure_or_skip() {
    local label="${1:?label required}"
    local detail="${2:?detail required}"
    if [ "$RERANK_REQUIRE_MODEL" = "1" ]; then
        e2e_log_assert_eq "$detail" "available" "$label"
        return 1
    fi
    e2e_log_note "$label skipped: $detail; set EE_E2E_RERANK_REQUIRE_MODEL=1 to fail closed"
    return 0
}

run_native_rerank_determinism_lane() {
    local embed_candidate="${EE_EMBED_MODEL_FIXTURE_DIR:-${EE_EMBED_MODEL_DIR:-${RERANK_ORIGINAL_HOME}/.local/share/ee/models}}"
    local embed_model_dir=""
    local rerank_home="${EPIC_WORKSPACE}/rerank-home"
    local model_fetch_json index_json index_status_json config_json fusion_json reranked_json
    local fusion_order reranked_order fusion_ids reranked_ids target_is_top
    local comparison_status

    if ! jq -en --arg tolerance "$RERANK_SCORE_TOLERANCE" \
        '$tolerance | tonumber | . >= 0' >/dev/null 2>&1; then
        e2e_log_assert_eq "$RERANK_SCORE_TOLERANCE" "non-negative number" \
            "rerank_score_tolerance_valid"
        return 1
    fi

    if [ ! -f "$RERANK_MODEL_ARCHIVE" ]; then
        rerank_input_failure_or_skip "rerank_model_archive_available" \
            "missing $RERANK_MODEL_ARCHIVE"
        return $?
    fi

    if [ "$RERANK_REQUIRE_REFERENCE" = "1" ] && [ -z "$RERANK_REFERENCE_VECTOR" ]; then
        e2e_log_assert_eq "missing EE_E2E_RERANK_REFERENCE_VECTOR" \
            "reference vector file" "rerank_reference_vector_required"
        return 1
    fi

    if [ -f "$embed_candidate/model.safetensors" ] \
        && [ -f "$embed_candidate/tokenizer.json" ] \
        && [ -f "$embed_candidate/config.json" ]; then
        embed_model_dir="$embed_candidate"
    elif [ -f "$embed_candidate/potion-multilingual-128M/model.safetensors" ] \
        && [ -f "$embed_candidate/potion-multilingual-128M/tokenizer.json" ] \
        && [ -f "$embed_candidate/potion-multilingual-128M/config.json" ]; then
        embed_model_dir="$embed_candidate/potion-multilingual-128M"
    elif [ -f "$embed_candidate/model2vec/potion-multilingual-128M/model.safetensors" ] \
        && [ -f "$embed_candidate/model2vec/potion-multilingual-128M/tokenizer.json" ] \
        && [ -f "$embed_candidate/model2vec/potion-multilingual-128M/config.json" ]; then
        embed_model_dir="$embed_candidate/model2vec/potion-multilingual-128M"
    fi
    if [ -z "$embed_model_dir" ]; then
        rerank_input_failure_or_skip "rerank_embedding_fixture_available" \
            "no model.safetensors below $embed_candidate"
        return $?
    fi
    if [ -n "$RERANK_REFERENCE_VECTOR" ] && [ ! -f "$RERANK_REFERENCE_VECTOR" ]; then
        e2e_log_assert_eq "missing $RERANK_REFERENCE_VECTOR" "reference vector file" \
            "rerank_reference_vector_available"
        return 1
    fi

    mkdir -p "$rerank_home"
    export HOME="$rerank_home"
    export EE_EMBED_DOWNLOAD=off
    export EE_EMBED_MODEL_DIR="$embed_model_dir"
    export FRANKENSEARCH_OFFLINE=1
    export FRANKENSEARCH_ALLOW_DOWNLOAD=0

    run_rerank_json "model_fetch" model fetch rerank-default \
        --from-file "$RERANK_MODEL_ARCHIVE" --json || return 1
    model_fetch_json="$RERANK_JSON_OUTPUT"
    if ! printf '%s' "$model_fetch_json" | jq -e '
        .data.schema == "ee.model_fetch.v1"
        and .data.modelId == "rerank-default-v1"
        and .data.modelPurpose == "reranker"
        and .data.registryEntry.status == "available"
    ' >/dev/null 2>&1; then
        e2e_log_assert_eq "invalid model fetch contract" \
            "ee.model_fetch.v1 reranker available" "rerank_model_fetch_contract"
        return 1
    fi
    e2e_log_assert_eq "true" "true" "rerank_model_fetch_contract"

    remember_rerank_fixture "seed_trap" semantic fact \
        "BD1NL13_RERANK_TRAP release release release format format checklist checklist cargo cargo clippy clippy, but this is a noisy lexical trap and not the Rust release policy target." || return 1
    remember_rerank_fixture "seed_target" procedural rule \
        "BD1NL13_RERANK_TARGET The correct Rust release policy says run cargo fmt --check and cargo clippy before publishing." || return 1
    remember_rerank_fixture "seed_noise_one" semantic fact \
        "BD1NL13_RERANK_NOISE_ONE Database migration notes cover index ownership and schema upgrade ordering." || return 1
    remember_rerank_fixture "seed_noise_two" semantic fact \
        "BD1NL13_RERANK_NOISE_TWO Onboarding screenshots and terminal color themes need a design review." || return 1
    remember_rerank_fixture "seed_noise_three" semantic fact \
        "BD1NL13_RERANK_NOISE_THREE Rust ownership and borrowing prevent memory safety errors at compile time." || return 1

    run_rerank_json "index_rebuild" index rebuild --json || return 1
    index_json="$RERANK_JSON_OUTPUT"
    if ! printf '%s' "$index_json" | jq -e \
        '(.data.documents_total // .data.documentsTotal // 0) >= 5' >/dev/null 2>&1; then
        e2e_log_assert_eq \
            "$(printf '%s' "$index_json" | jq -r '.data.documents_total // .data.documentsTotal // 0')" \
            ">=5" "rerank_index_document_count"
        return 1
    fi
    e2e_log_assert_eq "true" "true" "rerank_index_document_count"

    run_rerank_json "index_status" index status --json || return 1
    index_status_json="$RERANK_JSON_OUTPUT"
    if ! printf '%s' "$index_status_json" | jq -e '
        .data.embedding.schema == "ee.embedding_posture.v1"
        and .data.embedding.semantic == true
        and .data.embedding.fast_model_id == "potion-multilingual-128M"
        and .data.embedding.vector_coverage.embedded >= 5
        and .data.embedding.vector_coverage.total >= 5
        and all(.data.degraded[]; .code != "embed_model_unavailable")
    ' >/dev/null 2>&1; then
        e2e_log_assert_eq \
            "semantic=$(printf '%s' "$index_status_json" | jq -r '.data.embedding.semantic // false') model=$(printf '%s' "$index_status_json" | jq -r '.data.embedding.fast_model_id // "<missing>"')" \
            "semantic=true model=potion-multilingual-128M" \
            "rerank_semantic_embedding_contract"
        return 1
    fi
    e2e_log_assert_eq "true" "true" "rerank_semantic_embedding_contract"

    run_rerank_json "config_top_k" config set search.rerank_top_k 5 --json || return 1
    config_json="$RERANK_JSON_OUTPUT"
    e2e_log_note "rerank_top_k_config=$(printf '%s' "$config_json" | jq -c '.data // {}')"
    run_rerank_json "config_off" config set search.rerank off --json || return 1
    run_rerank_json "fusion_search" search "$RERANK_QUERY" \
        --limit 5 --relevance-floor 0 --explain --json || return 1
    fusion_json="$RERANK_JSON_OUTPUT"
    if ! printf '%s' "$fusion_json" | jq -e '
        .data.rerank.schema == "ee.rerank_posture.v1"
        and .data.rerank.mode == "fusion_only"
        and .data.rerank.configured == "off"
        and .data.rerank.topK == 5
        and .data.rerank.rerankScoreCount == 0
        and (.data.results | length) == 5
        and all(.data.results[]; (has("rerankScore") | not))
    ' >/dev/null 2>&1; then
        e2e_log_assert_eq "invalid fusion-only baseline" \
            "off/fusion_only/topK=5/results=5/no rerankScore" \
            "rerank_fusion_baseline_contract"
        return 1
    fi
    e2e_log_assert_eq "true" "true" "rerank_fusion_baseline_contract"

    run_rerank_json "config_auto" config set search.rerank auto --json || return 1
    run_rerank_json "active_search" search "$RERANK_QUERY" \
        --limit 5 --relevance-floor 0 --explain --json || return 1
    reranked_json="$RERANK_JSON_OUTPUT"
    if ! printf '%s' "$reranked_json" | jq -e '
        .data.rerank.schema == "ee.rerank_posture.v1"
        and .data.rerank.mode == "reranked"
        and .data.rerank.configured == "auto"
        and .data.rerank.topK == 5
        and .data.rerank.available == true
        and .data.rerank.rerankScoreCount == 5
        and (.data.results | length) == 5
        and all(.data.results[];
            .scoreKind == "reranked"
            and (.rerankScore | type) == "number"
            and .rerankScore >= 0 and .rerankScore <= 1
            and .score == .rerankScore)
        and .data.results[0].explanation.factors[0].name == "rerank"
        and all(.data.degraded[];
            .code != "rerank_model_unavailable"
            and .code != "embed_model_unavailable")
    ' >/dev/null 2>&1; then
        e2e_log_assert_eq \
            "mode=$(printf '%s' "$reranked_json" | jq -r '.data.rerank.mode // "<missing>"') count=$(printf '%s' "$reranked_json" | jq -r '.data.rerank.rerankScoreCount // 0')" \
            "mode=reranked count=5" "rerank_active_contract"
        return 1
    fi
    e2e_log_assert_eq "true" "true" "rerank_active_contract"

    fusion_order=$(printf '%s' "$fusion_json" | jq -c '[.data.results[].content]')
    reranked_order=$(printf '%s' "$reranked_json" | jq -c '[.data.results[].content]')
    fusion_ids=$(printf '%s' "$fusion_json" | jq -c '[.data.results[].memoryId] | sort')
    reranked_ids=$(printf '%s' "$reranked_json" | jq -c '[.data.results[].memoryId] | sort')
    target_is_top=$(printf '%s' "$reranked_json" | jq -r \
        '(.data.results[0].content // "") | startswith("BD1NL13_RERANK_TARGET")')
    if [ "$fusion_ids" != "$reranked_ids" ] \
        || [ "$fusion_order" = "$reranked_order" ] \
        || [ "$target_is_top" != "true" ]; then
        e2e_log_assert_eq \
            "same_ids=$([ "$fusion_ids" = "$reranked_ids" ] && printf true || printf false) order_changed=$([ "$fusion_order" != "$reranked_order" ] && printf true || printf false) target_top=$target_is_top" \
            "same_ids=true order_changed=true target_top=true" \
            "rerank_order_influenced"
        return 1
    fi
    e2e_log_assert_eq "true" "true" "rerank_order_influenced"

    run_3x_assert_identical "native_rerank_search_json" \
        search "$RERANK_QUERY" --limit 5 --relevance-floor 0 --explain --json || return 1

    printf '%s' "$reranked_json" | jq -S --arg query "$RERANK_QUERY" \
        --argjson fusionOrder "$fusion_order" '
        {
            schema: "ee.rerank_determinism.vector.v1",
            query: $query,
            fusionOnlyOrder: $fusionOrder,
            rerankedOrder: [.data.results[].content],
            rerankedScores: [.data.results[] | {content, rerankScore}]
        }
    ' >"$RERANK_VECTOR_OUT"
    comparison_status=$?
    if [ "$comparison_status" -ne 0 ] \
        || ! jq -e '.schema == "ee.rerank_determinism.vector.v1"' \
            "$RERANK_VECTOR_OUT" >/dev/null 2>&1; then
        e2e_log_assert_eq "vector_write_exit=$comparison_status" \
            "vector_write_exit=0" "rerank_vector_emitted"
        return 1
    fi
    e2e_log_assert_eq "true" "true" "rerank_vector_emitted"
    e2e_log_note "rerank_determinism_vector=$RERANK_VECTOR_OUT"

    if [ -n "$RERANK_REFERENCE_VECTOR" ]; then
        jq -e --argjson tolerance "$RERANK_SCORE_TOLERANCE" \
            --slurpfile reference "$RERANK_REFERENCE_VECTOR" '
            . as $actual
            | $reference[0] as $expected
            | $actual.schema == "ee.rerank_determinism.vector.v1"
            and $expected.schema == $actual.schema
            and $expected.query == $actual.query
            and $expected.fusionOnlyOrder == $actual.fusionOnlyOrder
            and $expected.rerankedOrder == $actual.rerankedOrder
            and ($expected.rerankedScores | length) == ($actual.rerankedScores | length)
            and all(range(0; ($actual.rerankedScores | length));
                $actual.rerankedScores[.].content == $expected.rerankedScores[.].content
                and (($actual.rerankedScores[.].rerankScore
                    - $expected.rerankedScores[.].rerankScore) | fabs) <= $tolerance)
        ' "$RERANK_VECTOR_OUT" >/dev/null 2>&1
        comparison_status=$?
        if [ "$comparison_status" -ne 0 ]; then
            e2e_log_assert_eq \
                "reference_mismatch tolerance=$RERANK_SCORE_TOLERANCE" \
                "same query/order; scores within tolerance" \
                "rerank_cross_platform_vector"
            return 1
        fi
        e2e_log_assert_eq "true" "true" "rerank_cross_platform_vector"
    else
        e2e_log_note \
            "rerank_reference_vector_not_set; emitted $RERANK_VECTOR_OUT for cross-platform comparison"
    fi

    return 0
}

run_native_rerank_determinism_lane
RERANK_LANE_STATUS=$?
e2e_log_note "native_rerank_determinism_lane_status=$RERANK_LANE_STATUS"
if [ "$RERANK_LANE_STATUS" -ne 0 ]; then
    exit "$RERANK_LANE_STATUS"
fi

# Teardown runs via trap; logs the asserts_pass/asserts_fail summary.
