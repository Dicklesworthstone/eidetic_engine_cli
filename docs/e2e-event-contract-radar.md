# E2E Event Contract Radar

> Audience: agents and maintainers wiring shell E2E evidence into release gates.
> Background: `bd-2ljka` and `bd-2ljka.1`.

The E2E event contract radar is a static report for scripts that emit
`ee.test_event.v1` rows. Its job is to make failure-path evidence visible
before a script becomes a hard release gate.

The report schema is `ee.e2e_event_contract_radar.v1` at
`docs/schemas/ee.e2e_event_contract_radar.v1.json`.

## Report Shape

```json
{
  "schema": "ee.e2e_event_contract_radar.v1",
  "generatedAt": "2026-06-04T00:00:00Z",
  "mode": "advisory",
  "verdict": "advisory_gap",
  "summary": {
    "scriptCount": 2,
    "passCount": 1,
    "advisoryGapCount": 1,
    "knownGapCount": 0,
    "failCount": 0,
    "notApplicableCount": 0,
    "failurePathCount": 3,
    "missingFailureVerdictCount": 1,
    "allowlistedGapCount": 0
  },
  "requirements": [],
  "matrix": [],
  "degraded": []
}
```

The schema uses closed objects. Unknown properties are invalid at the report,
matrix-row, coverage, failure-path, allowlist, and degraded-entry levels.

## Matrix Rows

Each scanned script contributes one row under `matrix[]`.

| Field | Meaning |
| --- | --- |
| `scriptPath` | Repository-relative shell or PowerShell script path under `scripts/` or `tests/`. |
| `scriptPathHash` | Redaction-safe hash of the path string. |
| `declaredEventSchemas` | Event schema ids detected in the script, usually `ee.test_event.v1`. |
| `status` | `pass`, `advisory_gap`, `known_gap`, `fail`, or `not_applicable`. |
| `coverage` | One enum per required evidence field. Values are `present`, `missing`, `not_applicable`, or `allowlisted`. |
| `failurePaths` | Branch-level evidence for early exits or command failures. |
| `allowlist` | Machine-readable owner, reason, and expiry for accepted temporary gaps. |

The required coverage fields are:

| Coverage field | Required evidence |
| --- | --- |
| `commandStart` | A `command_start` event or equivalent command-begin marker. |
| `commandEnd` | A `command_end` event with elapsed time and exit code when a command is run. |
| `assertOk` | Success verdict rows for assertions that prove the script did useful work. |
| `assertFailOrResult` | `assert_fail` or an equivalent `assert_result`/failure-verdict row on every early failure path. |
| `schemaValidationStatus` | Evidence that emitted JSON or JSONL was schema-checked, skipped with a reason, or not applicable. |
| `redactionStatus` | Evidence that stdout/stderr/log material was redacted, hashed, or proven non-sensitive. |
| `firstFailureDiagnosis` | A compact first-failure explanation suitable for Beads and Agent Mail handoff. |
| `stdoutArtifactPath` | Redaction-safe stdout artifact path or hash when stdout matters. |
| `stderrArtifactPath` | Redaction-safe stderr artifact path or hash when stderr matters. |
| `sanitizedEnv` | Evidence that environment variables were scrubbed or recorded without secrets. |

## Advisory And Blocking Modes

`mode = "advisory"` is the seeding mode. The radar emits a report and may
return success even when rows are `advisory_gap` or `known_gap`. Agents should
use the counts to pick remediation beads, not to claim a release gate passed.

`mode = "blocking"` is the release-gate mode. In blocking mode:

- `fail` rows fail the gate.
- `advisory_gap` rows fail the gate.
- `known_gap` rows are allowed only when the row has an active allowlist with
  an owner, reason, and future `expiresAt`.
- `not_applicable` rows are neutral and must still explain why the script does
  not emit `ee.test_event.v1` evidence.

## Verify Wiring

`scripts/verify.sh` runs the radar as `E2E Event Contract Radar Advisory`
after the contract-drift radar and before package artifact checks. The stage
writes `.e2e-event-contract-radar-report.json` by default. Set
`EE_E2E_EVENT_CONTRACT_RADAR_REPORT=/path/to/report.json` to change that path.

Advisory and known gaps do not fail verify while the radar is being seeded.
Scanner/runtime errors still fail the stage. Promotion to blocking mode should
wait until the scanner has fixture-backed coverage, no unknown high-severity
gaps remain, and at least one remediation pass has landed.

Optional allowlist input is controlled by
`EE_E2E_EVENT_CONTRACT_RADAR_ALLOWLIST` or `--allowlist <path>`. The file may be
a JSON array or an object with `entries[]`; each entry must include
`scriptPath`, `reason`, `owner`, and future `expiresAt`. Active entries convert
matching advisory/failing rows to `known_gap`; expired entries remain visible
as advisory/failing gaps.

## Fixtures

Fixtures live in `tests/fixtures/e2e_event_contract_radar/`.

- `complete_and_gap_report.json` is a positive fixture with one complete row
  and multiple rows missing failure verdicts.
- `extra_property_negative_report.json` is intentionally invalid. A schema
  validator must reject it because the top-level object contains an
  `unexpectedField` property.

`allowlist_example.json` documents the optional known-gap input shape.

Any future scanner should validate both fixtures before claiming schema
conformance. Cargo-backed validation must run through RCH on the Mac dev host;
do not use local Cargo fallback as evidence.

The no-Cargo golden harness is
`scripts/e2e_event_contract_radar_golden.sh`. It runs the static scanner over
the fixture scripts, normalizes `generatedAt`, compares the output to
`complete_and_gap_report.json`, and checks that the intentionally invalid
negative fixture still exercises the closed-object contract.
