#!/usr/bin/env bash
# bd-3qs2i.4.6 — F4.6 e2e for `embed_model_unavailable`.
#
# Exercises the embedder-only-down path against a real ee binary in a fresh
# workspace. The fault-injection knob is EE_EMBED_MODEL_PATH: pointing it at a
# missing model should leave lexical search available while surfacing the more
# precise embed_model_unavailable degraded code.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

if [ -z "${WORKSPACE:-}" ]; then
    workspace_root="$REPO_ROOT/tests/logs/agent_ergonomics_workspaces"
    mkdir -p "$workspace_root"
    WORKSPACE="$(mktemp -d "$workspace_root/ee-e2e-f4-XXXX")"
fi
export WORKSPACE

# shellcheck source=scripts/e2e_lib/agent_ergonomics_lib.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/agent_ergonomics_lib.sh"

missing_embed_model_path="$WORKSPACE/missing-embed-model/model.bin"
has_embed_model_unavailable='((.data.degraded // .degraded // []) | map(.code) | contains(["embed_model_unavailable"]))'
has_lexical_unavailable='((.data.degraded // .degraded // []) | map(.code) | contains(["lexical_unavailable"]))'
has_search_unavailable='((.data.degraded // .degraded // []) | map(.code) | contains(["search_unavailable"]))'

ee_workspace() {
    "$EE_BIN" --workspace "$WORKSPACE" "$@"
}

ee_workspace_with_missing_embedder() {
    EE_EMBED_MODEL_PATH="$missing_embed_model_path" ee_workspace "$@"
}

remember_search_memory() {
    local fact="${1:?fact required}"
    ee_workspace remember "$fact" \
        --level semantic \
        --kind fact \
        --json
}

# ---------------------------------------------------------------------------
log_step "Initialize fresh workspace"
init_out=$(ee_workspace init --json)
assert_jq "$init_out" '.success' "true" "init returns success=true"
assert_jq "$init_out" \
    '(.data.status == "created" or .data.status == "already_exists" or .data.status == "revalidated")' \
    "true" "init produced a usable workspace"

# ---------------------------------------------------------------------------
log_step "Verify workspace is fresh"
fresh_out=$(ee_workspace memory list --json)
assert_jq "$fresh_out" '(.data.memories // []) | length' "0" \
    "workspace has zero memories"

# ---------------------------------------------------------------------------
log_step "Seed lexical-search corpus"
for fact in \
    "Cargo workspace uses Rust nightly and remote-only RCH verification." \
    "FrankenSQLite indexes back local-first memory search." \
    "Embedding outages should degrade to lexical search without losing results."; do
    remember_out=$(remember_search_memory "$fact")
    assert_jq "$remember_out" '.success' "true" "remember search fact succeeds"
done

# ---------------------------------------------------------------------------
log_step "Force embedder unavailable with EE_EMBED_MODEL_PATH"
mkdir -p "$(dirname "$missing_embed_model_path")"
if [ -e "$missing_embed_model_path" ]; then
    record_failure "embed_fault_path_absent" "missing embed model path unexpectedly exists"
else
    record_pass "embed_fault_path_absent"
fi

# ---------------------------------------------------------------------------
log_step "Search still works via lexical fallback"
search_out=$(ee_workspace_with_missing_embedder search "cargo workspace" --json)
assert_jq "$search_out" '.success' "true" "search succeeds with missing embedder"
assert_jq "$search_out" '((.data.results // []) | length) > 0' "true" \
    "search returns lexical fallback results"
assert_jq "$search_out" "$has_embed_model_unavailable" "true" \
    "embed_model_unavailable appears in degraded[]"
assert_jq "$search_out" "$has_lexical_unavailable" "false" \
    "lexical_unavailable is absent"
assert_jq "$search_out" "$has_search_unavailable" "false" \
    "search_unavailable is absent"

# ---------------------------------------------------------------------------
log_step "Recovery details point at re-embedding"
assert_jq "$search_out" \
    '(.data.degraded[] | select(.code == "embed_model_unavailable") | .details.recovery[0].command)' \
    "ee index reembed --workspace ." "first recovery command is ee index reembed"
assert_jq "$search_out" \
    '(.data.degraded[] | select(.code == "embed_model_unavailable") | .details.recovery[1].command)' \
    "cargo build --features embed-quality" "second recovery command is embed-quality rebuild"
assert_jq "$search_out" \
    '(.data.degraded[] | select(.code == "embed_model_unavailable") | .details.lexicalAvailable)' \
    "true" "details.lexicalAvailable is true"

# ---------------------------------------------------------------------------
log_step "Context surfaces the same embedder degradation"
context_out=$(ee_workspace_with_missing_embedder context "cargo workspace info" \
    --max-tokens 2000 \
    --json)
assert_jq "$context_out" '.success' "true" "context succeeds with missing embedder"
assert_jq "$context_out" "$has_embed_model_unavailable" "true" \
    "context carries embed_model_unavailable"
assert_jq "$context_out" '((.data.pack.items // []) | length) > 0' "true" \
    "context pack still has lexical fallback items"

# ---------------------------------------------------------------------------
log_step "Restoring embedder path clears the degraded code"
restored_out=$(ee_workspace search "cargo workspace" --json)
assert_jq "$restored_out" "$has_embed_model_unavailable" "false" \
    "embed_model_unavailable absent after restoring default embedder path"

# ---------------------------------------------------------------------------
log_step "Tracing target fires under EE_LOG_JSON=1"
trace_log="$LOG_DIR/trace.log"
EE_EMBED_MODEL_PATH="$missing_embed_model_path" \
    EE_LOG_JSON=1 \
    RUST_LOG=ee::search::embedder_down=warn \
    "$EE_BIN" --workspace "$WORKSPACE" search "cargo workspace" --json \
    >"$LOG_DIR/trace_search.json" 2>"$trace_log"
assert_contains "$(cat "$trace_log")" "ee::search::embedder_down" \
    "tracing target ee::search::embedder_down emitted"
assert_contains "$(cat "$trace_log")" "embed_model_unavailable" \
    "trace event names embed_model_unavailable"
