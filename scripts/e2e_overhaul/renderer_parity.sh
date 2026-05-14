#!/usr/bin/env bash
# D8.1 — cross-renderer parity e2e driver.
#
# The Rust contract test (`tests/renderer_parity_matrix.rs`) owns the fast
# in-process matrix. This script exercises the real CLI over the J2 corpus and
# asserts that every context renderer carries the canonical pack identity and
# selected memory content unless the omission is documented in
# `tests/renderer_parity_omissions.toml`.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_D8_1_renderer_parity"
seed_corpus

hash_stdin() {
    if command -v blake3sum >/dev/null 2>&1; then
        blake3sum | awk '{print $1}'
    else
        shasum -a 256 | awk '{print $1}'
    fi
}

canonical_context_projection() {
    jq -S '{
        schema,
        success,
        query: .data.pack.query,
        hash: .data.pack.hash,
        budget: .data.pack.budget,
        items: (.data.pack.items // [] | map({
            rank,
            memoryId,
            section,
            content,
            why,
            selectedIn,
            provenanceCount: (.provenance // [] | length)
        })),
        skippedTotal: .data.pack.skippedTotal,
        degradedCodes: (.data.degraded // [] | map(.code) | sort)
    }'
}

CONTEXT_QUERY="renderer parity release workflow"
PACK_QUERY_FILE="$EPIC_WORKSPACE/renderer-parity.eeq.json"
cat > "$PACK_QUERY_FILE" <<JSON
{
  "schema": "ee.query.v1",
  "query": "$CONTEXT_QUERY",
  "budget": {"max_tokens": 1500, "candidate_pool": 32},
  "filters": {"tags": [], "levels": [], "kinds": []},
  "profile": "compact"
}
JSON

CONTEXT_JSON=$(ee_workspace context "$CONTEXT_QUERY" --max-tokens 1500 --format json 2>/dev/null || true)
if ! printf '%s' "$CONTEXT_JSON" | jq . >/dev/null 2>&1; then
    e2e_log_assert_eq "invalid" "parseable" "d8_1_context_json_parses"
    exit 0
fi

PACK_HASH=$(printf '%s' "$CONTEXT_JSON" | jq -r '.data.pack.hash // empty')
FIRST_CONTENT=$(printf '%s' "$CONTEXT_JSON" | jq -r '.data.pack.items[0].content // empty')
FIRST_MEMORY_ID=$(printf '%s' "$CONTEXT_JSON" | jq -r '.data.pack.items[0].memoryId // empty')
PROJECTION_HASH=$(printf '%s' "$CONTEXT_JSON" | canonical_context_projection | hash_stdin)
e2e_log_assert_eq "${PACK_HASH:+present}" "present" "d8_1_context_json_pack_hash_present"
e2e_log_assert_eq "${FIRST_MEMORY_ID:+present}" "present" "d8_1_context_json_first_memory_present"
e2e_log_assert_eq "${FIRST_CONTENT:+present}" "present" "d8_1_context_json_first_content_present"
e2e_log_note "d8_1_projection_hash=$PROJECTION_HASH pack_hash=$PACK_HASH first_memory=$FIRST_MEMORY_ID"

for FORMAT in human json toon jsonl compact hook markdown mermaid; do
    FORMAT_OUTPUT=$(ee_workspace context "$CONTEXT_QUERY" \
        --max-tokens 1500 --format "$FORMAT" 2>/dev/null || true)
    if printf '%s' "$FORMAT_OUTPUT" | grep -Fq "$PACK_HASH"; then
        e2e_log_assert_eq "true" "true" "d8_1_${FORMAT}_context_carries_pack_hash"
    else
        e2e_log_assert_eq "missing" "$PACK_HASH" "d8_1_${FORMAT}_context_carries_pack_hash"
    fi

    case "$FORMAT" in
        json|toon|jsonl|hook|markdown)
            if printf '%s' "$FORMAT_OUTPUT" | grep -Fq "$FIRST_CONTENT"; then
                e2e_log_assert_eq "true" "true" "d8_1_${FORMAT}_context_carries_item_content"
            else
                e2e_log_assert_eq "missing" "content" "d8_1_${FORMAT}_context_carries_item_content"
            fi
            ;;
        human|compact|mermaid)
            e2e_log_note "d8_1_${FORMAT}_content_omitted_per_registry=true"
            ;;
    esac
done

PACK_JSON=$(ee_workspace pack "$CONTEXT_QUERY" --max-tokens 1500 --format json 2>/dev/null || true)
if printf '%s' "$PACK_JSON" | jq . >/dev/null 2>&1; then
    PACK_JSON_HASH=$(printf '%s' "$PACK_JSON" | jq -r '.data.pack.hash // empty')
    e2e_log_assert_eq "$PACK_JSON_HASH" "$PACK_HASH" "d8_1_pack_alias_hash_matches_context"
else
    e2e_log_assert_eq "invalid" "parseable" "d8_1_pack_alias_json_parses"
fi

PACK_BUILD_JSON=$(ee_workspace pack build --query-file "$PACK_QUERY_FILE" \
    --format json 2>/dev/null || true)
if printf '%s' "$PACK_BUILD_JSON" | jq . >/dev/null 2>&1; then
    PACK_BUILD_PROJECTION_HASH=$(printf '%s' "$PACK_BUILD_JSON" \
        | canonical_context_projection | hash_stdin)
    e2e_log_assert_eq "${PACK_BUILD_PROJECTION_HASH:+present}" "present" \
        "d8_1_pack_build_projection_hash_present"
else
    e2e_log_assert_eq "invalid" "parseable" "d8_1_pack_build_json_parses"
fi

SEARCH_JSON=$(ee_workspace search "$CONTEXT_QUERY" --json 2>/dev/null || true)
if printf '%s' "$SEARCH_JSON" | jq . >/dev/null 2>&1; then
    SEARCH_PROJECTION_HASH=$(printf '%s' "$SEARCH_JSON" \
        | jq -S '{schema, success, query: .data.query, results: (.data.results // [] | map({id: (.id // .memoryId // .docId), content: (.content // .snippet // null), score: (.score // null)}))}' \
        | hash_stdin)
    e2e_log_assert_eq "${SEARCH_PROJECTION_HASH:+present}" "present" \
        "d8_1_search_projection_hash_present"
else
    e2e_log_assert_eq "invalid" "parseable" "d8_1_search_json_parses"
fi
