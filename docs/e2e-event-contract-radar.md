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

## Agent Workflow

Run the radar before promoting a shell E2E script to a release gate, after
touching a script that emits `ee.test_event.v1`, and when a failed E2E left too
little handoff evidence for Beads or Agent Mail.

```bash
scripts/e2e_event_contract_radar.sh \
  --json \
  --quiet \
  --output .e2e-event-contract-radar-report.json
```

For a focused remediation pass, scan only the script you plan to fix:

```bash
scripts/e2e_event_contract_radar.sh \
  --json \
  --quiet \
  --output /dev/null \
  scripts/e2e_overhaul/swarm_replay_lab_smoke.sh
```

Read `summary` first, then inspect `matrix[]` rows whose `status` is
`advisory_gap`, `known_gap`, or `fail`. A good closeout names both the focused
row result and the full-run summary. Example:

```text
focused radar: status=pass failurePaths=[]
full radar: scriptCount=173 passCount=1 advisoryGapCount=55
```

The radar is static. It proves that a script contains the expected failure
verdict pattern; it does not prove that the underlying `ee` binary is current or
that a Cargo-backed workflow passed. If a real smoke fails because the local
binary is stale, keep the emitted `assert_result` artifact as evidence and state
the stale-binary error explicitly. Refreshing the binary or running Rust tests
must go through RCH; local Cargo fallback is not acceptable proof on the Mac dev
host.

## Reading Verdicts

Use the row status to choose the next action:

| Status | Meaning | Agent action |
| --- | --- | --- |
| `pass` | Required fields and every detected failure path have verdict evidence. | Use as closeout evidence alongside shell/static checks. |
| `advisory_gap` | The script appears relevant but at least one required field or branch is missing. | Pick the smallest high-value failure path and add an `assert_fail` or `assert_result` row before the exit. |
| `known_gap` | The gap is temporarily accepted by an active allowlist entry. | Do not treat as done; cite owner, reason, and expiry, then prefer fixing it when nearby. |
| `fail` | The scanner or contract found a hard error. | Fix the scanner input or report before relying on the row. |
| `not_applicable` | The script does not emit `ee.test_event.v1` evidence. | Leave alone unless the script is being promoted to a logged E2E gate. |

The most useful field for remediation is `failurePaths[]`. Each entry points to
a direct `exit` or implicit failure such as `jq -e` under `set -e`. Fix the
branch at that location, not the whole script.

## Remediation Pattern

For shell scripts, each failure branch should emit one compact verdict row
before exiting. The row may be `assert_fail` when the script already uses a
shared logger, or `assert_result` when the script records command results and
assertion outcomes in one object. It must include:

- the sanitized command or validation step that failed;
- redaction-safe workspace and sanitized environment posture;
- elapsed time and exit code, using `0` only for checks that did not run a
  command;
- stdout and stderr artifact paths, or an explicit not-applicable posture;
- `schema_validation_status`, `redaction_status`, and
  `first_failure_diagnosis`.

Prefer a small helper function over copy-pasted JSON fragments when a script has
multiple exits. Keep diagnostics on stderr and machine data in JSON/JSONL
artifacts. Do not include raw workspace paths, secrets, mail bodies, memory
content, query text, or full command output in event rows.

## Examples

Fixture scripts under `tests/fixtures/e2e_event_contract_radar/scripts/` show
the common cases:

| Fixture | Verdict | Lesson |
| --- | --- | --- |
| `complete.sh` | `pass` | Direct exits are acceptable when each branch already emits verdict evidence with diagnosis and artifact paths. |
| `comment_only_event_reference.sh` | `not_applicable` | Comment-only references to `ee.test_event.v1` and evidence fields do not make a helper a logged E2E. |
| `success_only.sh` | `advisory_gap` | Success events are not enough; early failures need their own verdict row. |
| `set_e_implicit_exit.sh` | `advisory_gap` | `jq -e` can terminate a `set -e` script before an assertion row is written. Wrap it in `if ! jq -e ...; then emit verdict; fi`. |
| `cleanup_trap_only.sh` | `advisory_gap` | Cleanup traps do not replace command lifecycle and assertion evidence. |
| `no_event_logging.sh` | `not_applicable` | Plain shell helpers are neutral until they claim `ee.test_event.v1` evidence. |
| `allowlist_example.json` | `known_gap` input | Allowlisting is a temporary owner/reason/expiry record, not a pass. |

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

## Related Docs

- `docs/agent-ux/e2e-conventions.md` describes the shared shell logging
  conventions for agent-facing E2Es.
- `docs/agent-ux/swarm-replay-contracts.md` and
  `docs/agent-ux/workload-replay.md` describe the replay smoke that motivated
  the first remediation pass.
- `docs/testing-strategy.md` defines closeout expectations for code, behavior,
  docs-only, and shell E2E work.
