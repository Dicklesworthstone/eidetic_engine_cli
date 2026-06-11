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
#   dependency_xcheck  — current dependency contract prose matches accepted pins
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
SELF_TEST=0

usage() {
  cat <<'USAGE'
Usage: scripts/contract-drift-radar.sh [--json] [--quiet] [--strict] [--self-test] [--events-out <path>] [--output <path>]

  --json              Emit the JSON report to stdout (diagnostics on stderr).
  --quiet             Suppress human-readable summary (still writes JSON to disk).
  --strict            Exit code 4 if any violation is detected (default: advisory exit 0).
  --self-test         Validate the live report and per-phase event contract.
  --events-out <path> Append the per-phase ee.test_event.v1 JSONL to this file.
  --output <path>     Override .contract-drift-radar-report.json location.

Reads:
  docs/schemas/*.json            (canonical contracts)
  AGENTS.md, README.md, CLAUDE.md (if present)
  docs/external-derivation-operator.md, docs/agent-ux/*.md (current-facing docs)
  COMPREHENSIVE_PLAN.md
  docs/dependency-contract-matrix.md, docs/dependency-research-notes.md
  docs/degraded_codes.md         (degraded-code catalog)
  tests/fixtures/failure_modes/*.json (failure-mode fixtures)

Writes:
  .contract-drift-radar-report.json (schema "ee.contract_drift_radar.v1")

Exit codes: 0=advisory/self-test pass, 1=self-test failure, 4=violations detected (only with --strict).
USAGE
}

require_path_arg() {
  local flag="$1"
  if [ "$#" -lt 2 ] || [ -z "${2:-}" ] || [[ "${2:-}" == --* ]]; then
    printf 'contract-drift-radar: %s requires a path\n' "$flag" >&2
    exit 2
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --json) JSON_FLAG=1; shift ;;
    --quiet) QUIET=1; shift ;;
    --strict) STRICT=1; shift ;;
    --self-test) SELF_TEST=1; QUIET=1; shift ;;
    --events-out) require_path_arg "$@"; EVENTS_OUT="$2"; shift 2 ;;
    --output) require_path_arg "$@"; OUTPUT_PATH="$2"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 1 ;;
  esac
done

if ! command -v jq >/dev/null 2>&1; then
  echo "contract-drift-radar: jq required but not found" >&2
  exit 2
fi

generated_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
emitted_event_count=0
emitted_event_phases="[]"

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
  emitted_event_count=$((emitted_event_count + 1))
  emitted_event_phases=$(printf '%s' "$emitted_event_phases" | jq --arg phase "$phase" '. + [$phase]')
  if [ -n "$EVENTS_OUT" ]; then
    printf '%s\n' "$line" >>"$EVENTS_OUT"
  fi
}

run_self_test() {
  local report_json="$1"
  local phases_json="$2"
  local phase_count="$3"

  printf '%s\n' "$report_json" | jq -e '
    (.violations.docsScan | length) as $docs_scan_violations |
    (.violations.jsonExampleCheck | length) as $json_example_violations |
    (.violations.taxonomyXcheck | length) as $taxonomy_violations |
    (.violations.dependencyXcheck | length) as $dependency_violations |
    ($docs_scan_violations + $json_example_violations + $taxonomy_violations + $dependency_violations) as $total_violations |
    .schema == "ee.contract_drift_radar.v1" and
    .verdict == (if $total_violations > 0 then "violations" else "ok" end) and
    .summary.docsScanned > 0 and
    .summary.schemasLoaded > 0 and
    .summary.envelopeExamplesScanned > 0 and
    .summary.staleEnvelopeRefs == $docs_scan_violations and
    .summary.schemaIdViolations == $json_example_violations and
    .summary.documentedCodes > 0 and
    .summary.fixtureCodes > 0 and
    .summary.documentedMissingFixture == $taxonomy_violations and
    .summary.dependencyDocsChecked == (.summary.dependencyDocsCheckedFiles | length) and
    .summary.dependencyDocsChecked >= 1 and
    .summary.dependencyDocsCheckedFiles == [
      "Cargo.toml",
      "docs/dependency-research-notes.md",
      "docs/dependency-contract-matrix.md",
      "COMPREHENSIVE_PLAN.md"
    ] and
    .summary.dependencyProfileViolations == $dependency_violations
  ' >/dev/null || {
    printf 'self-test FAILED: report contract did not match expected live radar shape\n' >&2
    printf '%s\n' "$report_json" >&2
    exit 1
  }

  [ "$phase_count" -eq 6 ] || {
    printf 'self-test FAILED: expected 6 phase events, got %s\n' "$phase_count" >&2
    printf '%s\n' "$phases_json" >&2
    exit 1
  }

  printf '%s\n' "$phases_json" | jq -e '
    sort == [
      "dependency_xcheck",
      "docs_scan",
      "inventory_load",
      "json_example_check",
      "summary",
      "taxonomy_xcheck"
    ]
  ' >/dev/null || {
    printf 'self-test FAILED: phase event set drifted\n' >&2
    printf '%s\n' "$phases_json" >&2
    exit 1
  }

  printf 'self-test PASSED: contract drift radar report and phase events are stable\n'
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
  rel="${doc#"${ROOT}"/}"
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
  rel="${doc#"${ROOT}"/}"
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
  grep -oE "^## \`[a-z0-9_]+\`" "$DEGRADED_DOC" \
    | sed -E "s/^## \`//; s/\`$//" \
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

# ---- Phase 5: dependency_xcheck -------------------------------------------

# Static, intentionally narrow dependency-profile guard. This catches current
# docs that drift from Cargo.toml and the dependency contract matrix for the
# runtime substrate. The Cargo-backed forbidden-dependency tests remain the
# authoritative feature-tree proof.
DEPENDENCY_RESEARCH_DOC="${ROOT}/docs/dependency-research-notes.md"
DEPENDENCY_MATRIX_DOC="${ROOT}/docs/dependency-contract-matrix.md"
COMPREHENSIVE_PLAN_DOC="${ROOT}/COMPREHENSIVE_PLAN.md"
CARGO_TOML="${ROOT}/Cargo.toml"
accepted_asupersync_profile='asupersync = { version = "=0.3.4", default-features = false, features = ["tracing-integration"] }'
dependency_violations="[]"
dependency_violation_count=0
dependency_docs_checked=0
dependency_docs_checked_files="[]"

append_dependency_checked_file() {
  local file="$1"
  dependency_docs_checked_files=$(printf '%s' "$dependency_docs_checked_files" \
    | jq --arg file "$file" '. + [$file]')
  dependency_docs_checked=$((dependency_docs_checked + 1))
}

append_dependency_violation() {
  local file="$1"
  local line="$2"
  local code="$3"
  local context="$4"
  local expected="$5"
  local obj
  obj=$(jq -cn \
    --arg file "$file" \
    --arg line "$line" \
    --arg code "$code" \
    --arg context "$context" \
    --arg expected "$expected" \
    '{file: $file, line: ($line | tonumber? // 0), code: $code, context: $context, expected: $expected}')
  dependency_violations=$(printf '%s' "$dependency_violations" | jq --argjson obj "$obj" '. + [$obj]')
  dependency_violation_count=$((dependency_violation_count + 1))
}

if [ -f "$CARGO_TOML" ]; then
  append_dependency_checked_file "Cargo.toml"
  if ! grep -Fq "$accepted_asupersync_profile" "$CARGO_TOML"; then
    hit=$(grep -nE '^asupersync[[:space:]]*=' "$CARGO_TOML" 2>/dev/null | head -1 || true)
    append_dependency_violation \
      "Cargo.toml" \
      "$(printf '%s' "$hit" | awk -F: '{print $1}')" \
      "dependency_profile_drift" \
      "$(printf '%s' "$hit" | cut -d: -f2-)" \
      "$accepted_asupersync_profile"
  fi
fi

if [ -f "$DEPENDENCY_RESEARCH_DOC" ]; then
  append_dependency_checked_file "docs/dependency-research-notes.md"
  if ! grep -Fq "$accepted_asupersync_profile" "$DEPENDENCY_RESEARCH_DOC"; then
    hit=$(grep -nF 'asupersync = {' "$DEPENDENCY_RESEARCH_DOC" 2>/dev/null | head -1 || true)
    append_dependency_violation \
      "docs/dependency-research-notes.md" \
      "$(printf '%s' "$hit" | awk -F: '{print $1}')" \
      "dependency_profile_drift" \
      "$(printf '%s' "$hit" | cut -d: -f2-)" \
      "$accepted_asupersync_profile"
  fi
  while IFS= read -r hit; do
    [ -z "$hit" ] && continue
    append_dependency_violation \
      "docs/dependency-research-notes.md" \
      "$(printf '%s' "$hit" | awk -F: '{print $1}')" \
      "stale_dependency_profile_reference" \
      "$(printf '%s' "$hit" | cut -d: -f2-)" \
      "asupersync =0.3.4 with default-features=false and tracing-integration"
  done < <(grep -nE 'asupersync[^[:cntrl:]]*(0\.3\.[12])|0\.3\.[12][^[:cntrl:]]*asupersync|leave .{0,80}asupersync.{0,80}default features|\["test-internals", "proc-macros"\]' "$DEPENDENCY_RESEARCH_DOC" 2>/dev/null || true)
fi

if [ -f "$DEPENDENCY_MATRIX_DOC" ]; then
  append_dependency_checked_file "docs/dependency-contract-matrix.md"
  matrix_line=$(grep -nE "^\| \`asupersync\` \|" "$DEPENDENCY_MATRIX_DOC" 2>/dev/null | head -1 || true)
  if ! printf '%s' "$matrix_line" | grep -Fq "registry \`=0.3.4\`" \
    || ! printf '%s' "$matrix_line" | grep -Fq 'default-features = false' \
    || ! printf '%s' "$matrix_line" | grep -Fq 'tracing-integration'; then
    append_dependency_violation \
      "docs/dependency-contract-matrix.md" \
      "$(printf '%s' "$matrix_line" | awk -F: '{print $1}')" \
      "dependency_profile_drift" \
      "$(printf '%s' "$matrix_line" | cut -d: -f2-)" \
      "asupersync matrix row includes registry \`=0.3.4\`, \`default-features = false\`, and \`tracing-integration\`"
  fi
fi

if [ -f "$COMPREHENSIVE_PLAN_DOC" ]; then
  append_dependency_checked_file "COMPREHENSIVE_PLAN.md"
  plan_line=$(grep -nE '^asupersync[[:space:]]*=' "$COMPREHENSIVE_PLAN_DOC" 2>/dev/null | head -1 || true)
  if ! printf '%s' "$plan_line" | grep -Fq 'version = "=0.3.4"' \
    || ! printf '%s' "$plan_line" | grep -Fq 'default-features = false' \
    || ! printf '%s' "$plan_line" | grep -Fq 'tracing-integration'; then
    append_dependency_violation \
      "COMPREHENSIVE_PLAN.md" \
      "$(printf '%s' "$plan_line" | awk -F: '{print $1}')" \
      "dependency_profile_drift" \
      "$(printf '%s' "$plan_line" | cut -d: -f2-)" \
      "$accepted_asupersync_profile"
  fi
  while IFS= read -r hit; do
    [ -z "$hit" ] && continue
    append_dependency_violation \
      "COMPREHENSIVE_PLAN.md" \
      "$(printf '%s' "$hit" | awk -F: '{print $1}')" \
      "stale_dependency_profile_reference" \
      "$(printf '%s' "$hit" | cut -d: -f2-)" \
      "asupersync =0.3.4 with default-features=false and tracing-integration"
  done < <(grep -nE 'asupersync[^[:cntrl:]]*(0\.3\.[12]|version = "0\.3")|features = \["proc-macros"\]' "$COMPREHENSIVE_PLAN_DOC" 2>/dev/null || true)
fi

dependency_status="ok"
[ "$dependency_violation_count" -gt 0 ] && dependency_status="violations"
emit_event "dependency_xcheck" "$dependency_status" \
  "cross-checked accepted dependency profile prose against Cargo.toml" \
  "$(jq -cn --argjson f "$dependency_docs_checked" --argjson files "$dependency_docs_checked_files" --argjson v "$dependency_violation_count" \
    '{dependencyDocsChecked: $f, dependencyDocsCheckedFiles: $files, dependencyProfileViolations: $v}')" \
  "[]"

# ---- Phase 6: summary ------------------------------------------------------

total_violations=$((stale_count + example_violation_count + taxonomy_orphan_count + dependency_violation_count))
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
  --argjson dependency "$dependency_violations" \
  --argjson stale_count "$stale_count" \
  --argjson example_count "$example_count" \
  --argjson example_violation_count "$example_violation_count" \
  --argjson example_skipped_legacy "$example_skipped_legacy" \
  --argjson taxonomy_orphan_count "$taxonomy_orphan_count" \
  --argjson documented_codes "$documented_codes" \
  --argjson fixture_codes "$fixture_codes" \
  --argjson dependency_docs_checked "$dependency_docs_checked" \
  --argjson dependency_docs_checked_files "$dependency_docs_checked_files" \
  --argjson dependency_violation_count "$dependency_violation_count" \
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
      documentedMissingFixture: $taxonomy_orphan_count,
      dependencyDocsChecked: $dependency_docs_checked,
      dependencyDocsCheckedFiles: $dependency_docs_checked_files,
      dependencyProfileViolations: $dependency_violation_count
    },
    schemaInventory: $schema_ids,
    violations: {
      docsScan: $stale,
      jsonExampleCheck: $examples,
      taxonomyXcheck: $taxonomy,
      dependencyXcheck: $dependency
    }
  }')

printf '%s\n' "$report" >"$OUTPUT_PATH"

emit_event "summary" "$verdict" \
  "contract-drift-radar verdict: $verdict ($total_violations violations across phases)" \
  "$(jq -cn --argjson t "$total_violations" '{totalViolations: $t}')" \
  "[]"

if [ "$SELF_TEST" -eq 1 ]; then
  run_self_test "$report" "$emitted_event_phases" "$emitted_event_count"
  exit 0
fi

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
    printf '  dependency docs checked: %s\n' "$dependency_docs_checked"
    printf '  dependency profile violations: %s\n' "$dependency_violation_count"
  } >&2
fi

if [ "$STRICT" -eq 1 ] && [ "$total_violations" -gt 0 ]; then
  exit 4
fi

exit 0
