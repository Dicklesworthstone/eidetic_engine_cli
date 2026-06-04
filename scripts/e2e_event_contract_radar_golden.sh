#!/usr/bin/env bash
# bd-2ljka.3 - no-Cargo golden harness for the e2e event-contract radar.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCANNER="$ROOT/scripts/e2e_event_contract_radar.sh"
FIXTURE_ROOT="$ROOT/tests/fixtures/e2e_event_contract_radar/scripts"
GOLDEN="$ROOT/tests/fixtures/e2e_event_contract_radar/complete_and_gap_report.json"
NEGATIVE="$ROOT/tests/fixtures/e2e_event_contract_radar/extra_property_negative_report.json"
SCHEMA="$ROOT/docs/schemas/ee.e2e_event_contract_radar.v1.json"
FIXED_GENERATED_AT="2026-06-04T00:00:00Z"

actual_report() {
  "$SCANNER" --json --quiet --output /dev/null --scripts-root "$FIXTURE_ROOT" |
    jq --arg generatedAt "$FIXED_GENERATED_AT" '.generatedAt = $generatedAt'
}

diff -u "$GOLDEN" <(actual_report) >&2

actual_report |
  jq -e '
    .schema == "ee.e2e_event_contract_radar.v1"
    and .verdict == "advisory_gap"
    and .summary == {
      scriptCount: 5,
      passCount: 1,
      advisoryGapCount: 3,
      knownGapCount: 0,
      failCount: 0,
      notApplicableCount: 1,
      failurePathCount: 5,
      missingFailureVerdictCount: 3,
      allowlistedGapCount: 0
    }
    and (.matrix | map(.scriptPath)) == [
      "tests/fixtures/e2e_event_contract_radar/scripts/cleanup_trap_only.sh",
      "tests/fixtures/e2e_event_contract_radar/scripts/complete.sh",
      "tests/fixtures/e2e_event_contract_radar/scripts/no_event_logging.sh",
      "tests/fixtures/e2e_event_contract_radar/scripts/set_e_implicit_exit.sh",
      "tests/fixtures/e2e_event_contract_radar/scripts/success_only.sh"
    ]
    and (
      .matrix[]
      | select(.scriptPath == "tests/fixtures/e2e_event_contract_radar/scripts/complete.sh")
      | .status == "pass"
        and ([.failurePaths[].assertFailOrResult] | all(. == "present"))
    )
    and (
      .matrix[]
      | select(.scriptPath == "tests/fixtures/e2e_event_contract_radar/scripts/success_only.sh")
      | .status == "advisory_gap"
        and .coverage.assertFailOrResult == "missing"
        and .coverage.firstFailureDiagnosis == "missing"
    )
  ' >/dev/null

jq -e '
  .additionalProperties == false
  and ."$defs".matrixRow.additionalProperties == false
  and ."$defs".coverage.additionalProperties == false
  and ."$defs".failurePath.additionalProperties == false
  and ."$defs".allowlist.additionalProperties == false
' "$SCHEMA" >/dev/null

jq -e '
  .schema == "ee.e2e_event_contract_radar.v1"
  and has("unexpectedField")
' "$NEGATIVE" >/dev/null

jq -e 'has("unexpectedField") | not' "$GOLDEN" >/dev/null

printf 'e2e_event_contract_radar_golden: pass\n' >&2
