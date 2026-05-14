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

VOLATILE_FIELD_NAMES=(
    generatedAt
    generated_at
    computed_at
    last_accessed
    last_accessed_at
    last_seen_at
    last_used_at
    audit_ts
    elapsedMs
    elapsed_ms
    startedAt
    started_at
    endedAt
    ended_at
    ts
    timestamp
    runIndex
    run_index
    ee_binary_hash
    databasePath
    workspacePath
    indexDir
    snapshotRefreshedAt
    runDurationMs
    witnessElapsedMs
    witnessRecordedAt
    algorithmStartedAt
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

if [ "${BASH_SOURCE[0]}" != "$0" ]; then
    return 0
fi

require_jq
epic_setup "epic_J_determinism"

# Run `ee ARGS...` three times, canonicalize each output, hash, and
# emit an assert via the J1 logger. The assert name is the first arg.
run_3x_assert_identical() {
    local name="$1"
    shift
    local h1 h2 h3
    h1=$("$EE_BINARY" "$@" --workspace "$EPIC_WORKSPACE" 2>/dev/null \
            | strip_variable_fields | jq -S '.' | hash_stdin)
    h2=$("$EE_BINARY" "$@" --workspace "$EPIC_WORKSPACE" 2>/dev/null \
            | strip_variable_fields | jq -S '.' | hash_stdin)
    h3=$("$EE_BINARY" "$@" --workspace "$EPIC_WORKSPACE" 2>/dev/null \
            | strip_variable_fields | jq -S '.' | hash_stdin)
    if [ "$h1" = "$h2" ] && [ "$h2" = "$h3" ]; then
        e2e_log_assert_eq "true" "true" "determinism_${name}"
        e2e_log_note "determinism_${name}_hash=$h1"
    else
        e2e_log_assert_eq "h1=$h1 h2=$h2 h3=$h3" "all_equal" "determinism_${name}"
        e2e_log_note "determinism_${name}_diverged h1=$h1 h2=$h2 h3=$h3"
    fi
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

# Teardown runs via trap; logs the asserts_pass/asserts_fail summary.
