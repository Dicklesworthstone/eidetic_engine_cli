#!/usr/bin/env bash
# bd-ppbue.29 - no-Cargo golden harness for panic-helper radar reports.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCANNER="$ROOT/scripts/panic-helper-radar.sh"
FIXTURE_ROOT="$ROOT/tests/fixtures/panic_helper_radar"
FIXTURE_REL_ROOT="tests/fixtures/panic_helper_radar"
SCHEMA="$ROOT/docs/schemas/ee.panic_helper_radar.v1.json"
PASS_GOLDEN="$FIXTURE_ROOT/file_level_allow_pass_report.json"
FAIL_GOLDEN="$FIXTURE_ROOT/expect_unwrap_fail_report.json"
NO_INPUT_GOLDEN="$FIXTURE_ROOT/no_dirty_inputs_report.json"
FIXED_GENERATED_AT="2026-06-06T00:00:00Z"

pass_report() {
  "$SCANNER" --json --quiet --output /dev/null \
    "$FIXTURE_REL_ROOT/file_level_allow.rs" |
    jq --arg generatedAt "$FIXED_GENERATED_AT" '.generatedAt = $generatedAt'
}

fail_report() {
  "$SCANNER" --json --quiet --advisory --output /dev/null \
    "$FIXTURE_REL_ROOT/expect_fail.rs" \
    "$FIXTURE_REL_ROOT/unwrap_fail.rs" \
    "$FIXTURE_REL_ROOT/not_rust.txt" |
    jq --arg generatedAt "$FIXED_GENERATED_AT" '.generatedAt = $generatedAt'
}

diff -u "$PASS_GOLDEN" <(pass_report) >&2
diff -u "$FAIL_GOLDEN" <(fail_report) >&2

pass_report |
  jq -e '
    .schema == "ee.panic_helper_radar.v1"
    and .verdict == "pass"
    and .summary.scannedFileCount == 1
    and .summary.violationCount == 0
    and .summary.skippedCount == 0
    and (.violations | length) == 0
    and (.skipped | length) == 0
  ' >/dev/null

fail_report |
  jq -e '
    .schema == "ee.panic_helper_radar.v1"
    and .verdict == "fail"
    and .summary.scannedFileCount == 2
    and .summary.violationCount == 2
    and .summary.skippedCount == 1
    and ([.violations[].lint] | sort) == ["expect_used", "unwrap_used"]
    and ([.violations[].helper] | sort) == ["expect", "unwrap_err"]
    and (.skipped[0].reason == "not_rust_file")
  ' >/dev/null

jq -e '
  .schema == "ee.panic_helper_radar.v1"
  and .verdict == "pass"
  and .summary == {
    scannedFileCount: 0,
    violationCount: 0,
    skippedCount: 0,
    scannedPaths: []
  }
  and (.violations | length) == 0
  and (.skipped | length) == 0
' "$NO_INPUT_GOLDEN" >/dev/null

jq -e '
  .additionalProperties == false
  and .["$defs"].summary.additionalProperties == false
  and .["$defs"].violation.additionalProperties == false
  and .["$defs"].skipped.additionalProperties == false
' "$SCHEMA" >/dev/null

jq -e '
  .properties.schema.const == "ee.panic_helper_radar.v1"
  and .properties.verdict.enum == ["pass", "fail"]
  and .["$defs"].violation.properties.helper.enum == ["expect", "expect_err", "unwrap", "unwrap_err"]
  and .["$defs"].violation.properties.lint.enum == ["expect_used", "unwrap_used"]
' "$SCHEMA" >/dev/null

printf 'panic_helper_radar_golden: pass\n' >&2
