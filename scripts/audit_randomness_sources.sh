#!/usr/bin/env bash
# N4.1 (bd-17c65.14.4.1) — randomness source audit.
#
# Walks src/ via ripgrep and emits an `ee.audit.randomness_inventory.v1`
# JSON inventory of every ambient-randomness call site. The inventory
# is the input to N4.2 (Deterministic<Seed> token design): N4.2's bead
# body must cite this inventory by content_hash before opening
# implementation work.
#
# Usage:
#   ./scripts/audit_randomness_sources.sh
#     -> writes tests/randomness_inventory.json (machine-readable)
#     -> writes docs/perf-forensics/randomness_audit_<DATE>.md (human-readable)
#
#   ./scripts/audit_randomness_sources.sh /tmp/audit.json
#     -> writes the inventory to the given path (no human-readable file)
#
# The script is read-only against src/; it does not edit any code.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC_DIR="${SRC_DIR:-$REPO_ROOT/src}"
OUTPUT="${1:-$REPO_ROOT/tests/randomness_inventory.json}"
DATE_STAMP="$(date -u +%Y-%m-%d)"
TIMESTAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
HUMAN_REPORT="$REPO_ROOT/docs/perf-forensics/randomness_audit_${DATE_STAMP}.md"

# Tooling required.
for tool in rg jq git; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "audit_randomness_sources: required tool '$tool' not found in PATH" >&2
        exit 2
    fi
done

SRC_COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo 'no-commit')"

mkdir -p "$(dirname "$OUTPUT")"
mkdir -p "$(dirname "$HUMAN_REPORT")"

ROWS_FILE="$(mktemp -t ee_randomness_rows.XXXXXX.jsonl)"
trap 'rm -f "$ROWS_FILE"' EXIT

emit_rows() {
    local kind="$1"
    local severity="$2"
    local remediation="$3"
    local pattern="$4"
    # rg outputs file:line:content. `rg` exits 1 on no-match, and the
    # grep -v filters can also empty the pipeline; under `pipefail` +
    # `set -e` that aborts the whole script. We swallow non-match exits
    # because an empty result is a valid (zero-hit) audit row set.
    {
        rg --no-heading --line-number --type rust --regexp "$pattern" "$SRC_DIR" 2>/dev/null \
            | { grep -vE '^[^:]*/tests/' || true; } \
            | { grep -vE '^[^:]*/snapshots/' || true; } \
            || true
    } | while IFS=: read -r file line rest; do
            # Trim leading whitespace and bound the excerpt at 200 chars.
            excerpt="$(printf '%s' "$rest" | sed 's/^[[:space:]]*//' | cut -c1-200)"
            # Make file path relative to the repo root.
            rel_file="${file#"$REPO_ROOT/"}"
            jq -c -n \
                --arg fn_path "$rel_file" \
                --argjson line "$line" \
                --arg call_excerpt "$excerpt" \
                --arg randomness_kind "$kind" \
                --arg severity "$severity" \
                --arg proposed_remediation "$remediation" \
                '{fn_path: $fn_path, line: $line, call_excerpt: $call_excerpt, randomness_kind: $randomness_kind, severity: $severity, proposed_remediation: $proposed_remediation}'
        done >> "$ROWS_FILE"
}

# rng — ambient RNG sources
emit_rows rng latent_risk capability_token 'rand::(thread_rng|random|Rng|SeedableRng|RngCore|rngs::)'
emit_rows rng latent_risk capability_token 'fastrand::'
# ring::rand::SystemRandom is the crypto RNG; classify as
# deterministic_today since cryptographic non-determinism is expected
# and the surface is bounded (key generation only).
emit_rows rng deterministic_today capability_token 'ring::rand::SystemRandom'

# systemtime — wall-clock reads
emit_rows systemtime latent_risk inject_clock 'SystemTime::now'
emit_rows systemtime latent_risk inject_clock 'Instant::now'
emit_rows systemtime confirmed_drift inject_clock 'chrono::Utc::now|Utc::now\(\)'

# hashmap_iter — non-deterministic iteration order. Flag every
# HashMap declaration as latent_risk; downstream sort may already
# resolve the non-determinism, but the audit requires manual review.
emit_rows hashmap_iter latent_risk sort_iter 'HashMap<'

# env — ambient environment reads
emit_rows env latent_risk env_var 'std::env::var\b'
emit_rows env latent_risk env_var 'std::env::var_os'

# filesystem_order — read_dir without sort. Manual review needed to
# distinguish "sorted downstream" from "iterated as-is".
emit_rows filesystem_order latent_risk manual_sort 'fs::read_dir\b|std::fs::read_dir'

# ulid_clock — ULID generation paths read SystemTime internally.
# Classified as confirmed_drift because output IDs ARE non-deterministic
# without an injected clock; this is the primary N4.3 refactor target.
emit_rows ulid_clock confirmed_drift capability_token 'ulid::Generator|Ulid::new\b|::generate\(\)'

ROW_COUNT="$(wc -l < "$ROWS_FILE" | tr -d ' ')"

# Build the envelope JSON.
jq -s --arg schema "ee.audit.randomness_inventory.v1" \
      --arg generated_at "$TIMESTAMP" \
      --arg src_commit "$SRC_COMMIT" \
      --argjson count_total "$ROW_COUNT" \
      '{
        schema: $schema,
        generated_at: $generated_at,
        src_commit: $src_commit,
        count_total: $count_total,
        rows: .,
        summary: {
          by_kind: (group_by(.randomness_kind) | map({(.[0].randomness_kind): length}) | add // {}),
          by_severity: (group_by(.severity) | map({(.[0].severity): length}) | add // {}),
          by_remediation: (group_by(.proposed_remediation) | map({(.[0].proposed_remediation): length}) | add // {})
        }
      }' "$ROWS_FILE" > "$OUTPUT"

# Compute a stable content_hash that N4.2 can cite. We hash the rows
# (not the wrapping metadata) so timestamp drift does not invalidate
# downstream citations.
ROWS_HASH="$(jq -c '.rows | sort_by(.fn_path, .line, .randomness_kind)' "$OUTPUT" | shasum -a 256 | awk '{print $1}')"
# Patch the hash into the output as a top-level field.
jq --arg h "blake3-ish:$ROWS_HASH" '. + {rows_content_hash: $h}' "$OUTPUT" > "$OUTPUT.tmp" \
    && mv "$OUTPUT.tmp" "$OUTPUT"

# Only emit the human-readable report when invoked with default output
# path (treat custom outputs as machine consumers).
if [ "$OUTPUT" = "$REPO_ROOT/tests/randomness_inventory.json" ]; then
    {
        echo "# Randomness source audit — ${DATE_STAMP}"
        echo
        echo "Owner: bd-17c65.14.4.1 (N4.1). Auto-generated by"
        echo "\`scripts/audit_randomness_sources.sh\`."
        echo
        echo "- Source commit: \`${SRC_COMMIT}\`"
        echo "- Generated at: \`${TIMESTAMP}\`"
        echo "- Total findings: **${ROW_COUNT}**"
        echo
        echo "## Summary"
        echo
        echo "### By randomness kind"
        echo
        jq -r '.summary.by_kind | to_entries | sort_by(-.value) | map("- `\(.key)`: \(.value)")[]' "$OUTPUT"
        echo
        echo "### By severity"
        echo
        jq -r '.summary.by_severity | to_entries | sort_by(-.value) | map("- `\(.key)`: \(.value)")[]' "$OUTPUT"
        echo
        echo "### By proposed remediation"
        echo
        jq -r '.summary.by_remediation | to_entries | sort_by(-.value) | map("- `\(.key)`: \(.value)")[]' "$OUTPUT"
        echo
        echo "## Top 10 file paths by hit count"
        echo
        jq -r '.rows | group_by(.fn_path) | map({path: .[0].fn_path, hits: length}) | sort_by(-.hits) | .[:10] | map("- `\(.path)`: \(.hits) findings")[]' "$OUTPUT"
        echo
        echo "## Notes"
        echo
        echo "- This audit is a static-grep first pass. Each row's"
        echo "  \`fn_path\` is the source file (not the enclosing Rust"
        echo "  fn) because grep does not give us syntactic context;"
        echo "  N4.3 (the threading refactor) can use ast-grep to lift"
        echo "  rows to true qualified paths if needed."
        echo "- \`hashmap_iter\` rows enumerate every \`HashMap<\`"
        echo "  declaration; some iterations are sorted downstream and"
        echo "  do not leak determinism. Manual review per row decides"
        echo "  the actual remediation."
        echo "- \`ring::rand::SystemRandom\` is classified"
        echo "  \`deterministic_today\` because its crypto-key surface"
        echo "  is intentionally non-deterministic and bounded."
        echo "- \`rows_content_hash\` in the inventory pins the"
        echo "  sorted-rows hash. N4.2's bead body cites this hash to"
        echo "  prove the design references this specific audit revision."
        echo
        echo "## Cited from"
        echo
        echo "- bd-17c65.14.4.1 — N4.1 owner bead."
        echo "- bd-17c65.14.4.2 — N4.2 (Deterministic<Seed> token);"
        echo "  cites this audit by \`rows_content_hash\` before"
        echo "  opening implementation."
        echo "- bd-17c65.10.7 — J7 determinism harness; cross-references"
        echo "  this audit when extending its \`_strip_fields\` catalog."
    } > "$HUMAN_REPORT"
    echo "wrote $OUTPUT ($ROW_COUNT rows)"
    echo "wrote $HUMAN_REPORT"
    echo "rows_content_hash: $ROWS_HASH"
else
    echo "wrote $OUTPUT ($ROW_COUNT rows)"
fi
