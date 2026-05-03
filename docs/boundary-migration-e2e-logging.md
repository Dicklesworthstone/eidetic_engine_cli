# Boundary Migration E2E Logging

This document defines the reusable log contract for mechanical-boundary
migration tests. It extends the artifact rules in `docs/testing-strategy.md` for
the command families that are being split into deterministic CLI surfaces and
agent-side skills.

## Schema

Each logged command step uses logical schema `ee.e2e.boundary_log.v1`. A test may
store the record as one JSON file or as the same fields spread across a dossier,
but the following fields are required:

| Field | Meaning |
| --- | --- |
| `schema` | Always `ee.e2e.boundary_log.v1`. |
| `command` | Binary or command family, for example `ee`. |
| `argv` | Exact argument vector passed to the process. |
| `cwd` | Process working directory. |
| `workspace` | Resolved workspace root or `null` when not applicable. |
| `env_sanitized` | Environment overrides with secret values redacted. |
| `started_at_unix_ms` | Start time in Unix milliseconds. |
| `ended_at_unix_ms` | End time in Unix milliseconds. |
| `elapsed_ms` | Measured command duration. |
| `exit_code` | Process exit code, or `null` if unavailable. |
| `stdout_artifact_path` | Path to captured stdout bytes. |
| `stderr_artifact_path` | Path to captured stderr bytes. |
| `stdout_json_valid` | Whether stdout parsed as the requested machine format. |
| `schema_validation` | Expected schema, observed schema, and validation status. |
| `golden_validation` | Golden path and status, or `not_applicable`. |
| `redaction_status` | Redaction status plus classes observed or applied. |
| `evidence_ids` | Evidence or fixture IDs used by the command. |
| `degradation_codes` | Stable degradation codes observed in the response. |
| `mutation_summary` | `read_only`, `dry_run_no_mutation_expected`, `durable_write_expected`, or a narrower summary. |
| `command_boundary_matrix_row` | Reference to the command-boundary matrix row in `docs/mechanical-boundary-command-inventory.md`, or `null` if not applicable. |
| `fixture_hashes` | Map of fixture IDs to content hashes, or empty when no fixtures are required. |
| `db_generation_before` | DB generation number before command execution, or `null` when not applicable. |
| `db_generation_after` | DB generation number after command execution, or `null` when not applicable. |
| `index_generation_before` | Search/index generation before command execution, or `null` when not applicable. |
| `index_generation_after` | Search/index generation after command execution, or `null` when not applicable. |
| `runtime_budget` | Runtime budget in milliseconds if configured, or `null` when unbounded. |
| `cancellation_status` | `not_applicable`, `not_requested`, `requested`, `completed`, or `timeout`. |
| `reproduction_command` | Stable shell command for re-running the exact step from `cwd` with sanitized overrides. |
| `first_failure` | `null` on success; otherwise the shortest actionable diagnosis. |

## Validation

Boundary logs must fail fast on trust-boundary violations:

- JSON-mode stdout must contain only the requested machine data.
- Human diagnostics, progress, tracing, and debug lines belong in stderr or
  explicit artifact files.
- Parsed stdout schema must match the expected command contract.
- Missing or stale golden checks must be explicit in `golden_validation`.
- Redaction status must be explicit even when no redaction was needed.
- Mutating commands must state whether mutation was expected, dry-run-only, or
  denied.
- Command surfaces must have a corresponding entry in the command-boundary
  matrix (`docs/mechanical-boundary-command-inventory.md`).
- Read-only commands must not change DB generation; a mismatch is
  `unexpected_mutation`.
- Commands that require fixture evidence must include all fixture hashes in
  `fixture_hashes`; missing hashes are `missing_fixture_hash:<fixture_id>`.
- `env_sanitized` must prove sensitive values were omitted or redacted. Raw
  values for keys containing `SECRET`, `TOKEN`, `PASSWORD`, `KEY`, or
  `CREDENTIAL` are invalid in boundary logs.
- `reproduction_command` must be renderable without chat context and must not
  contain raw secrets.

Recommended `first_failure` values are stable short codes with one detail field:

| Code | When |
| --- | --- |
| `stdout_pollution` | stdout has non-machine text before, after, or instead of the machine payload. |
| `stdout_json_invalid:<reason>` | stdout is intended to be JSON but cannot be parsed. |
| `schema_mismatch:<observed>` | parsed `schema` differs from the expected schema. |
| `missing_required_field:<field>` | a required log field is absent. |
| `env_not_redacted:<key>` | sanitized env includes a raw value for a sensitive key. |
| `missing_matrix_row:<surface>` | command surface has no entry in the command-boundary matrix. |
| `unexpected_mutation` | command marked as `read_only` but DB generation changed. |
| `missing_fixture_hash:<fixture_id>` | fixture is required but its hash is absent from `fixture_hashes`. |
| `missing_reproduction_command` | log cannot render a command for local reproduction. |
| `error.code=<code>` | command returned a structured `ee.error.v1` payload. |

## Artifact Layout

Use the existing dossier convention unless a bead documents a compatible
alternative:

```text
target/ee-e2e/<scenario>/<run-id>/<step>/
├── boundary-log.json
├── command.txt
├── cwd.txt
├── workspace.txt
├── env.sanitized.json
├── stdout
├── stderr
├── stdout.schema.json
├── summary.json
├── redaction-report.json
├── degradation-report.json
└── first-failure.md
```

CI may upload the whole `<run-id>` directory as an artifact. Local repro should
be possible from `command.txt`, `cwd.txt`, `workspace.txt`, and
`env.sanitized.json` without reading chat history.

Each dossier may also include `summary.json` with schema
`ee.e2e.boundary_log.summary.v1`. The summary is a compact index over one or
more step logs and must sort step entries by `command` and `argv` so CI diffs are
stable.

## Required References

Command-family beads under the mechanical-boundary epic should cite this file in
their acceptance criteria when they add or update real-binary e2e coverage. At a
minimum, the lab, causal, learn, procedure, preflight, tripwire, recorder,
rehearse, certificate/claims, economy, situation/plan, memory revise, and status
health migration beads must use this schema for their logged command steps.
