#!/usr/bin/env bash
# contract-drift-radar.sh — static contract-drift radar (bd-31nul.5).
#
# Cargo-free scanner that runs the static portion of the public contract
# drift radar over current-facing agent documentation. Designed to fit the
# existing verification pipeline alongside scripts/bridge-staleness.sh and
# scripts/plan-drift.sh, and to fail-soft on missing tooling so it can run
# under RCH-blocked environments.
#
# Phases (each emits one ee.test_event.v1 JSONL line to stderr):
#   inventory_load     — enumerate docs/schemas/*.json (canonical contracts)
#   docs_scan          — current-facing docs reference live schema versions
#   json_example_check — current-facing JSONC envelope examples carry a live schema id
#   taxonomy_xcheck    — every degraded code in docs/degraded_codes.md has a
#                        tests/fixtures/failure_modes/<code>.json fixture
#   summary            — counts and overall verdict
#
# Output:
#   .contract-drift-radar-report.json     (schema "ee.contract_drift_radar.v1")
#   Event log on stderr (also persisted via --events-out <path>)
#
# Exit code: 0 advisory unless --strict is passed, then 4 on any violation.
#
# Cargo-backed proof remains the canonical verification surface
# (`cargo test -p ee --test contracts -- --include-ignored
#  schema_drift::current_envelope_examples_validate`); see
# `docs/contract-drift-radar.md` for the RCH command template.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_PATH="${ROOT}/.contract-drift-radar-report.json"
EVENTS_OUT=""
STRICT=0
QUIET=0
JSON_FLAG=0

usage() {
  cat <<'USAGE'
Usage: scripts/contract-drift-radar.sh [--json] [--quiet] [--strict] [--events-out <path>] [--output <path>]

  --json              Emit the JSON report to stdout (diagnostics on stderr).
  --quiet             Suppress human-readable summary (still writes JSON to disk).
  --strict            Exit code 4 if any violation is detected (default: advisory exit 0).
  --events-out <path> Append the per-phase ee.test_event.v1 JSONL to this file.
  --output <path>     Override .contract-drift-radar-report.json location.

Reads:
  docs/schemas/*.json            (canonical contracts)
  AGENTS.md, README.md, CLAUDE.md (if present)
  docs/external-derivation-operator.md, docs/agent-ux/*.md (current-facing docs)
  docs/degraded_codes.md         (degraded-code catalog)
  tests/fixtures/failure_modes/*.json (failure-mode fixtures)

Writes:
  .contract-drift-radar-report.json (schema "ee.contract_drift_radar.v1")

Exit codes: 0=advisory pass, 4=violations detected (only with --strict).
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --json) JSON_FLAG=1; shift ;;
    --quiet) QUIET=1; shift ;;
    --strict) STRICT=1; shift ;;
    --events-out) EVENTS_OUT="${2:-}"; shift 2 ;;
    --output) OUTPUT_PATH="${2:-}"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 1 ;;
  esac
done

if ! command -v jq >/dev/null 2>&1; then
  echo "contract-drift-radar: jq required but not found" >&2
  exit 2
fi

generated_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

emit_event() {
  local phase="$1"
  local status="$2"
  local message="$3"
  local count_json="${4:-{\}}"
  local degraded="${5:-[]}"
  local line
  line=$(jq -cn \
    --arg schema "ee.test_event.v1" \
    --arg surface "contract_drift_radar" \
    --arg bead_id "bd-31nul.5" \
    --arg phase "$phase" \
    --arg status "$status" \
    --arg workspace "$ROOT" \
    --arg message "$message" \
    --argjson counts "$count_json" \
    --argjson degraded "$degraded" \
    '{
      schema: $schema,
      surface: $surface,
      beadId: $bead_id,
      phase: $phase,
      status: $status,
      workspace: $workspace,
      message: $message,
      counts: $counts,
      degradedCodes: $degraded
    }')
  printf '%s\n' "$line" >&2
  if [ -n "$EVENTS_OUT" ]; then
    printf '%s\n' "$line" >>"$EVENTS_OUT"
  fi
}

if [ -n "$EVENTS_OUT" ]; then
  : >"$EVENTS_OUT"
fi

# ---- Phase 1: inventory_load ------------------------------------------------

SCHEMAS_DIR="${ROOT}/docs/schemas"
schema_ids="[]"
schema_count=0
if [ -d "$SCHEMAS_DIR" ]; then
  schema_ids=$(find "$SCHEMAS_DIR" -maxdepth 1 -type f -name 'ee.*.json' -print 2>/dev/null \
    | awk -F'/' '{print $NF}' \
    | sed 's/\.json$//' \
    | sort -u \
    | jq -R . \
    | jq -s 'unique')
  schema_count=$(printf '%s' "$schema_ids" | jq 'length')
fi

emit_event "inventory_load" "ok" "loaded canonical schema ids from docs/schemas" \
  "$(jq -cn --argjson n "$schema_count" '{schemasLoaded: $n}')" "[]"

# ---- Phase 2: docs_scan -----------------------------------------------------

# Current-facing files: NOT in docs/archive/, NOT *_LEGACY*, NOT comprehensive
# plan history blocks. The radar scans these for v1 envelope references that
# have been bumped to v2 elsewhere.
declare -a CURRENT_DOCS=()
for candidate in \
  "${ROOT}/AGENTS.md" \
  "${ROOT}/README.md" \
  "${ROOT}/CLAUDE.md" \
  "${ROOT}/docs/external-derivation-operator.md" \
  "${ROOT}/docs/agent_integration.md" \
  "${ROOT}/docs/contract-drift-radar.md" \
  "${ROOT}/docs/migration-guide.md"
do
  [ -f "$candidate" ] && CURRENT_DOCS+=("$candidate")
done
while IFS= read -r -d '' f; do
  CURRENT_DOCS+=("$f")
done < <(find "${ROOT}/docs/agent-ux" -maxdepth 1 -type f -name '*.md' -print0 2>/dev/null)

# Live envelope versions (source of truth: docs/schemas/ee.*.v*.json filenames).
# A "stale" reference is "ee.response.v1" or "ee.error.v1" appearing in a
# current-facing doc when the live version is v2.
stale_hits="[]"
stale_count=0
allow_marker='<!-- contract-drift-allow:'
for doc in "${CURRENT_DOCS[@]}"; do
  rel="${doc#${ROOT}/}"
  while IFS= read -r hit; do
    [ -z "$hit" ] && continue
    line_no=$(printf '%s' "$hit" | awk -F: '{print $1}')
    matched=$(printf '%s' "$hit" | cut -d: -f2-)
    # Skip allow-listed lines (callers can pin historical references)
    if printf '%s' "$matched" | grep -Fq "$allow_marker"; then
      continue
    fi
    # Skip explicit ARCHIVED / HISTORICAL prose lines
    if printf '%s' "$matched" | grep -qiE '(archived|historical|deprecated|legacy)'; then
      continue
    fi
    obj=$(jq -cn \
      --arg file "$rel" \
      --arg line "$line_no" \
      --arg context "$matched" \
      --arg code "stale_envelope_version_reference" \
      '{file: $file, line: ($line | tonumber? // 0), code: $code, context: $context}')
    stale_hits=$(printf '%s' "$stale_hits" | jq --argjson obj "$obj" '. + [$obj]')
    stale_count=$((stale_count + 1))
  done < <(grep -nE '\bee\.(response|error)\.v1\b' "$doc" 2>/dev/null || true)
done

docs_status="ok"
[ "$stale_count" -gt 0 ] && docs_status="violations"
emit_event "docs_scan" "$docs_status" \
  "scanned ${#CURRENT_DOCS[@]} current-facing docs for stale envelope versions" \
  "$(jq -cn --argjson n "$stale_count" --argjson f "${#CURRENT_DOCS[@]}" '{docsScanned: $f, staleEnvelopeRefs: $n}')" \
  "[]"

# ---- Phase 3: json_example_check -------------------------------------------

# Static check: each ```json/```jsonc fence in a current-facing doc that
# *names* "schema" at top-level should reference a schema id present in
# the inventory. We don't deep-validate here (the schema_drift.rs cargo
# test does that via JSONC normalization); we only fail when a documented
# example pins a schema id that no longer ships.
example_violations="[]"
example_count=0
example_violation_count=0
example_skipped_legacy=0
for doc in "${CURRENT_DOCS[@]}"; do
  rel="${doc#${ROOT}/}"
  # Pull fenced blocks via awk
  in_block=0
  block=""
  block_lang=""
  start_line=0
  line_no=0
  prev_line=""
  block_is_legacy=0
  while IFS= read -r line; do
    line_no=$((line_no + 1))
    if [ "$in_block" -eq 0 ]; then
      if printf '%s' "$line" | grep -qE '^```(json|jsonc)\s*$'; then
        in_block=1
        block=""
        block_lang=$(printf '%s' "$line" | sed -E 's/^```//; s/\s*$//')
        start_line=$line_no
        # Allow-list markers immediately preceding the fence:
        # <!-- legacy-example --> or <!-- contract-drift-allow:... -->
        block_is_legacy=0
        if printf '%s' "$prev_line" | grep -qE '<!--\s*(legacy-example|contract-drift-allow:)' ; then
          block_is_legacy=1
        fi
      fi
      prev_line="$line"
      continue
    fi
    if printf '%s' "$line" | grep -qE '^```\s*$'; then
      # End of fence — check the block
      in_block=0
      # Only inspect blocks that mention "schema":"ee.*"
      if printf '%s' "$block" | grep -qE '"schema"\s*:\s*"ee\.'; then
        schema_id=$(printf '%s' "$block" | grep -oE '"schema"\s*:\s*"ee\.[^"]+"' | head -1 | sed -E 's/.*"(ee\.[^"]+)".*/\1/')
        example_count=$((example_count + 1))
        if [ "$block_is_legacy" -eq 1 ]; then
          example_skipped_legacy=$((example_skipped_legacy + 1))
        elif [ -n "$schema_id" ]; then
          # Check inventory
          known=$(printf '%s' "$schema_ids" | jq -r --arg id "$schema_id" 'any(. == $id)')
          if [ "$known" != "true" ]; then
            obj=$(jq -cn \
              --arg file "$rel" \
              --arg line "$start_line" \
              --arg schema_id "$schema_id" \
              --arg lang "$block_lang" \
              --arg code "json_example_schema_id_unknown" \
              '{file: $file, line: ($line | tonumber? // 0), code: $code, schemaId: $schema_id, fenceLanguage: $lang}')
            example_violations=$(printf '%s' "$example_violations" | jq --argjson obj "$obj" '. + [$obj]')
            example_violation_count=$((example_violation_count + 1))
          fi
        fi
      fi
      block=""
      prev_line="$line"
      continue
    fi
    block="$(printf '%s\n%s' "$block" "$line")"
    prev_line="$line"
  done < "$doc"
done

examples_status="ok"
[ "$example_violation_count" -gt 0 ] && examples_status="violations"
emit_event "json_example_check" "$examples_status" \
  "validated JSONC envelope examples against schema inventory" \
  "$(jq -cn --argjson n "$example_count" --argjson v "$example_violation_count" --argjson s "$example_skipped_legacy" \
    '{envelopeExamplesScanned: $n, schemaIdViolations: $v, skippedLegacyExamples: $s}')" \
  "[]"

# ---- Phase 4: taxonomy_xcheck ---------------------------------------------

DEGRADED_DOC="${ROOT}/docs/degraded_codes.md"
FIXTURE_DIR="${ROOT}/tests/fixtures/failure_modes"
taxonomy_orphans="[]"
taxonomy_orphan_count=0
documented_codes=0
fixture_codes=0
if [ -f "$DEGRADED_DOC" ] && [ -d "$FIXTURE_DIR" ]; then
  # Codes from the catalog are H2 headings: '## `<code>`'
  doc_codes_tmp=$(mktemp)
  grep -oE '^## `[a-z0-9_]+`' "$DEGRADED_DOC" \
    | sed -E 's/^## `//; s/`$//' \
    | sort -u >"$doc_codes_tmp"
  documented_codes=$(wc -l <"$doc_codes_tmp" | tr -d '[:space:]')

  fixture_codes_tmp=$(mktemp)
  find "$FIXTURE_DIR" -maxdepth 1 -type f -name '*.json' \
    | awk -F'/' '{print $NF}' \
    | sed 's/\.json$//' \
    | sort -u >"$fixture_codes_tmp"
  fixture_codes=$(wc -l <"$fixture_codes_tmp" | tr -d '[:space:]')

  # Codes documented but missing a fixture
  while IFS= read -r code; do
    [ -z "$code" ] && continue
    if ! grep -Fxq "$code" "$fixture_codes_tmp"; then
      obj=$(jq -cn --arg c "$code" --arg code "documented_code_missing_fixture" \
        '{code: $code, degradedCode: $c}')
      taxonomy_orphans=$(printf '%s' "$taxonomy_orphans" | jq --argjson obj "$obj" '. + [$obj]')
      taxonomy_orphan_count=$((taxonomy_orphan_count + 1))
    fi
  done <"$doc_codes_tmp"

  rm -f "$doc_codes_tmp" "$fixture_codes_tmp"
fi

taxonomy_status="ok"
[ "$taxonomy_orphan_count" -gt 0 ] && taxonomy_status="violations"
emit_event "taxonomy_xcheck" "$taxonomy_status" \
  "cross-checked degraded-code documentation against failure-mode fixtures" \
  "$(jq -cn --argjson d "$documented_codes" --argjson f "$fixture_codes" --argjson o "$taxonomy_orphan_count" \
    '{documentedCodes: $d, fixtureCodes: $f, documentedMissingFixture: $o}')" \
  "[]"

# ---- Phase 5: summary ------------------------------------------------------

total_violations=$((stale_count + example_violation_count + taxonomy_orphan_count))
verdict="ok"
[ "$total_violations" -gt 0 ] && verdict="violations"

report=$(jq -n \
  --arg schema "ee.contract_drift_radar.v1" \
  --arg generated_at "$generated_at" \
  --arg verdict "$verdict" \
  --argjson schema_ids "$schema_ids" \
  --argjson stale "$stale_hits" \
  --argjson examples "$example_violations" \
  --argjson taxonomy "$taxonomy_orphans" \
  --argjson stale_count "$stale_count" \
  --argjson example_count "$example_count" \
  --argjson example_violation_count "$example_violation_count" \
  --argjson example_skipped_legacy "$example_skipped_legacy" \
  --argjson taxonomy_orphan_count "$taxonomy_orphan_count" \
  --argjson documented_codes "$documented_codes" \
  --argjson fixture_codes "$fixture_codes" \
  --argjson docs_scanned "${#CURRENT_DOCS[@]}" \
  '{
    schema: $schema,
    generatedAt: $generated_at,
    verdict: $verdict,
    summary: {
      docsScanned: $docs_scanned,
      schemasLoaded: ($schema_ids | length),
      staleEnvelopeRefs: $stale_count,
      envelopeExamplesScanned: $example_count,
      schemaIdViolations: $example_violation_count,
      skippedLegacyExamples: $example_skipped_legacy,
      documentedCodes: $documented_codes,
      fixtureCodes: $fixture_codes,
      documentedMissingFixture: $taxonomy_orphan_count
    },
    schemaInventory: $schema_ids,
    violations: {
      docsScan: $stale,
      jsonExampleCheck: $examples,
      taxonomyXcheck: $taxonomy
    }
  }')

printf '%s\n' "$report" >"$OUTPUT_PATH"

emit_event "summary" "$verdict" \
  "contract-drift-radar verdict: $verdict ($total_violations violations across phases)" \
  "$(jq -cn --argjson t "$total_violations" '{totalViolations: $t}')" \
  "[]"

if [ "$JSON_FLAG" -eq 1 ]; then
  printf '%s\n' "$report"
fi

if [ "$QUIET" -ne 1 ]; then
  {
    printf 'Contract Drift Radar -> %s\n' "$OUTPUT_PATH"
    printf '  verdict: %s\n' "$verdict"
    printf '  docs scanned: %s\n' "${#CURRENT_DOCS[@]}"
    printf '  schemas loaded: %s\n' "$schema_count"
    printf '  stale envelope refs: %s\n' "$stale_count"
    printf '  envelope examples scanned: %s\n' "$example_count"
    printf '  schema id violations: %s\n' "$example_violation_count"
    printf '  documented codes: %s\n' "$documented_codes"
    printf '  fixture codes: %s\n' "$fixture_codes"
    printf '  documented codes missing fixture: %s\n' "$taxonomy_orphan_count"
  } >&2
fi

if [ "$STRICT" -eq 1 ] && [ "$total_violations" -gt 0 ]; then
  exit 4
fi

exit 0
