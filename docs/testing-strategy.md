# Testing And Logging Strategy

This document is the project-wide verification contract for `ee`. It turns the
testing requirements in AGENTS.md, README.md, and the comprehensive plan into a
stable implementation target for future beads.

`ee` is an agent memory substrate, so correctness means more than "the command
ran." Tests must prove that the CLI stays local-first, deterministic,
explainable, safe under degraded dependencies, and useful to coding agents that
consume machine-readable output.

## Non-Negotiable Invariants

Every test plan, fixture, and closeout should preserve these invariants:

- stdout is data only: JSON, TOON, JSONL, or another explicit machine contract.
- stderr is diagnostics only: progress, warnings, tracing, and human context.
- JSON output uses a versioned schema field and stable field names.
- Human-readable output has a machine-equivalent JSON contract.
- Fixed inputs produce fixed output ordering, IDs, hashes, timestamps, and
  degradation metadata.
- Every retrieved or packed memory has provenance and an explanation path.
- Degraded dependencies are visible, structured, and actionable.
- Mutating commands are explicit and auditable; dry-run and idempotency are
  required where retries or partial failure are plausible.
- Secrets and private evidence are redacted before storage, before indexing,
  before rendering, and before artifact export.
- The default dependency tree excludes Tokio, tokio-util, hyper, axum, tower,
  reqwest, async-std, smol, rusqlite, SQLx, Diesel, SeaORM, and petgraph.
- Tests that exercise async behavior use Asupersync deterministic runtime
  support rather than introducing another executor.

## Test Family Matrix

| Family | Location | Purpose | Required For |
| --- | --- | --- | --- |
| Unit | Inline `#[cfg(test)]` modules | Prove local logic, edge cases, and invariants close to implementation | Domain models, scoring, packing, redaction, ID parsing, path handling |
| Contract | `tests/contracts/` | Freeze integration contracts with franken-stack dependencies and public adapters | SQLModel/FrankenSQLite, Asupersync, Frankensearch, CASS robot JSON, graph, TOON, MCP |
| Integration | `tests/integration/` or focused files under `tests/` | Run real binary flows against isolated local workspaces | `init`, `remember`, `search`, `context`, `why`, `status`, curation, import |
| Golden | `tests/golden/` | Prevent accidental output drift | JSON envelopes, Markdown packs, TOON output, diagnostics, error shapes |
| Evaluation | `tests/fixtures/eval/` plus `ee eval` tests | Prove that context and search help with real agent scenarios | Release, debug, migration, degraded, redaction, graph and stale-rule scenarios |
| Runtime Lab | `tests/contracts/asupersync_*.rs` | Prove cancellation, budgets, quiescence, and no orphan work | Imports, indexing, pack building, steward jobs, daemon paths |
| Property/Fuzz | `tests/fuzz/` or property modules | Explore parser and invariant spaces that example tests miss | Query schema, config, JSONL headers, IDs, redaction, pack budgets |
| Dependency Audit | CI and `tests/contracts/dependency_contract_matrix.rs` | Catch forbidden transitive dependencies and feature drift | Every feature profile and release gate |
| Replay/Repro | `tests/repro/`, `tests/golden/repro/`, future `ee repro` | Make failures and demos inspectable after the fact | Evaluation fixtures, claims, shadow runs, counterfactual lab |

Unit tests are mandatory for new domain logic. Public CLI, MCP, hook, renderer,
export, or evaluation behavior also requires at least one contract, integration,
golden, or evaluation test that exercises the visible surface through a real
command or protocol boundary.

## Fixture Taxonomy

Fixture names are stable handles. A future agent should be able to search for a
fixture by scenario name and understand what behavior it protects.

Core workspace fixtures:

- `empty_workspace`: no config, no database, no indexes.
- `fresh_workspace`: initialized workspace with no memories.
- `manual_memory`: explicit memories only, no CASS or semantic backend.
- `stale_index`: database generation ahead of search index generation.
- `offline_degraded`: no CASS, no semantic model, no network assumption.
- `locked_writer`: write lock held or queued while reads continue.
- `migration_required`: old schema requiring safe migration messaging.

Evaluation fixtures:

- `release_failure`: prior release mistake appears before release work.
- `async_migration`: Asupersync guidance outranks generic async advice.
- `ci_clippy_failure`: repeated CI failures become procedural memory.
- `dangerous_cleanup`: high-severity anti-patterns are pinned.
- `secret_redaction`: sensitive evidence never leaks into packs or artifacts.
- `stale_rule`: contradicted or aged rules are demoted or flagged.
- `graph_linked_decision`: graph proximity improves explanations without
  dominating retrieval.
- `conflicting_evidence`: trust and provenance make contradictions visible.
- `false_alarm`: warning feedback can reduce noise without deleting evidence.
- `procedure_drift`: reusable procedure fails revalidation without disappearing.
- `causal_confounding`: apparent memory utility is marked under-identified.

Adapter fixtures:

- `cass/v1`: stable CASS robot/search/view/expand/capabilities outputs.
- `agent-detect/codex` and `agent-detect/claude`: deterministic local
  installation and root detection.
- `mcp/stdio`: initialize, tools list, and read-only tool call transcripts.
- `toon`: JSON parity and malformed TOON error cases.

## Public Output Contracts

Every data-producing command must have a JSON contract before it is treated as
stable. Golden files should include both successful and degraded responses.

Required envelope fields for agent-native responses:

- `schema`: versioned schema name such as `ee.response.v1`.
- `command`: canonical command path and mode.
- `success`: boolean success marker.
- `data`: command-specific data when successful.
- `error`: structured error when unsuccessful.
- `degraded`: structured degradation list, even if empty.
- `provenance`: references for selected memories, evidence, fixtures, or
  artifacts where relevant.

Error contracts must include:

- stable `code`
- human-readable `message`
- `severity`
- `repair` or `next_action` when a safe action exists
- structured `details` for machine inspection

Human Markdown, compact text, TOON, Mermaid, MCP, hook, and future renderer
outputs are adapters over the canonical domain data. They must not change
ranking, filtering, redaction, provenance, degradation decisions, or exit codes.

## E2E Artifact Logging

End-to-end and integration tests that run a binary command must capture an
artifact dossier under a deterministic scenario directory. The implementation
may choose the exact run ID format, but the recommended shape is:

```text
target/ee-e2e/<scenario>/<run-id>/
├── command.txt
├── cwd.txt
├── env.sanitized.json
├── exit-code.txt
├── elapsed-ms.txt
├── stdout
├── stderr
├── stdout.schema.json
├── stderr.events.jsonl
├── redaction-report.json
├── degradation-report.json
└── first-failure.md
```

Required fields:

- command argv as executed
- cwd and resolved `--workspace`
- sanitized environment overrides
- toolchain and feature profile when relevant
- elapsed time and timeout budget
- exit code
- stdout artifact path and parse/schema status
- stderr artifact path and progress/tracing status
- redaction status and any redacted classes
- degradation status and repair command
- fixture ID, schema version, and golden file path
- concise first-failure diagnosis

Tests must assert that stdout parses as the requested machine format and that no
progress bars, tracing, warnings, or debug text appear there. stderr assertions
should allow diagnostics but require stable structure for JSONL progress events
once those events exist.

### Boundary-Migration Log Schema

Mechanical-boundary migration tests must use `ee.e2e.boundary_log.v1` as the
logical schema for each logged command step, even when a test stores the fields
in several small artifact files instead of one JSON object. The schema is a
cross-cutting contract for beads that split Rust CLI mechanics from agent-skill
workflows.

Required command-step fields:

- `command` and `argv`: the exact binary and arguments executed.
- `cwd` and `workspace`: the process directory and resolved workspace root.
- `env_sanitized`: environment override names and redacted values only.
- `started_at_unix_ms`, `ended_at_unix_ms`, and `elapsed_ms`.
- `exit_code`.
- `stdout_artifact_path` and `stderr_artifact_path`.
- `stdout_json_valid` and `schema_validation`.
- `golden_validation`.
- `redaction_status`.
- `evidence_ids`.
- `degradation_codes`.
- `mutation_summary`: `read_only`, `dry_run_no_mutation_expected`,
  `durable_write_expected`, or a more specific conservative summary.
- `command_boundary_matrix_row`: reference to the command-boundary matrix row
  in `docs/mechanical-boundary-command-inventory.md`, or null if not applicable.
- `fixture_hashes`: map of fixture IDs to content hashes, or empty when no
  fixtures are required.
- `db_generation_before` and `db_generation_after`: DB generation numbers
  before/after command execution, or null when not applicable.
- `runtime_budget`: runtime budget in milliseconds if configured, or null when
  unbounded.
- `cancellation_status`: `not_applicable`, `not_requested`, `requested`,
  `completed`, or `timeout`.
- `reproduction_command`: stable shell command for re-running the exact step
  from `cwd` with sanitized overrides.
- `first_failure`: null on a clean step; otherwise the shortest actionable
  diagnosis, such as `stdout_pollution`, `schema_mismatch:<schema>`,
  `missing_matrix_row:<surface>`, `unexpected_mutation`,
  `missing_fixture_hash:<fixture_id>`, `error.code=<code>`, or the first stable
  stderr line.

Boundary-migration e2e tests must fail if a successful JSON-mode command writes
human diagnostics to stdout, if stdout cannot be parsed as the requested machine
format, or if the parsed schema does not match the expected command contract.
Later mechanical-boundary command-family beads should cite this section in their
acceptance criteria and store CI artifacts under `target/ee-e2e/` or a
documented compatible subdirectory.

## Degradation Matrix

Graceful degradation is part of the product contract, not a best-effort message.
Each case needs a stable code, a test fixture, useful output status, and a repair
or next action.

Initial matrix:

| Code | Scenario | Expected Behavior |
| --- | --- | --- |
| `cass_unavailable` | CASS binary missing or unhealthy | Explicit memories still work; import/session evidence is marked unavailable |
| `semantic_disabled` | embedding backend missing or intentionally off | Lexical search still works and response says semantic is absent |
| `search_index_stale` | DB generation exceeds index generation | Command returns stale-index metadata and rebuild repair |
| `graph_snapshot_stale` | graph snapshot missing or older than DB | Retrieval works without graph boost and reports staleness |
| `agent_detector_unavailable` | local agent detection not available | Core memory commands still work; status reports missing adapter |
| `science_backend_unavailable` | optional analytics disabled | Simple deterministic metrics remain available where planned |
| `diagram_backend_unavailable` | diagram renderer unavailable | Canonical JSON stays available; diagram output reports adapter state |
| `redaction_applied` | output contains redacted evidence | Placeholder is stable and redaction metadata is present |
| `external_adapter_schema_mismatch` | CASS or MCP fixture version drift | Fail loudly with adapter/version details, no partial durable mutation |
| `lock_contention` | writer lock held | Reads continue where safe; writes queue, fail, or advise retry explicitly |

No degraded response may silently look complete. If a result is useful but
partial, the output must say which capability was missing and what that means.

## Determinism Rules

Tests must make nondeterminism impossible to miss:

- Use fixed clocks in fixtures.
- Use fixed IDs or deterministic ID providers.
- Use fixed seeds for ranking, graph, MMR, fuzz reproductions, and LabRuntime.
- Sort all equal-rank results by stable tie-breakers.
- Hash context packs from canonical serialized data, not renderer text.
- Store expected index and pack generations in golden fixtures.
- Record feature profile and dependency matrix in artifact dossiers.
- Require schema and golden updates in the same change when public output
  intentionally changes.

Deterministic tests must not depend on wall-clock network calls, paid LLM APIs,
ambient user configuration, or a mutable global CASS corpus. Use explicit
fixtures and temporary workspaces.

## Redaction And Privacy Tests

Redaction tests cover storage, indexing, rendering, artifact export, and replay.
At minimum, fixtures must include:

- API keys and bearer tokens.
- JWT-like strings.
- passwords in URLs and assignment forms.
- private key blocks.
- SSH keys and host credentials.
- cloud credentials.
- prompt-injection-looking memory content.
- large private excerpts that must be summarized or withheld.

Assertions:

- raw secret text is absent from DB exports, indexes, stdout, stderr, and
  artifact dossiers.
- placeholders are stable enough for golden tests.
- redaction classes are reported in output metadata.
- remote-model or optional semantic paths never receive private evidence unless
  an explicit future policy says they may.
- quarantined or instruction-like evidence is never rendered as authoritative
  procedural advice.

## Forbidden Dependency Checks

The default feature profile and every release-relevant optional profile must be
audited. CI should run the equivalent of:

```bash
cargo tree -e features
cargo tree -e normal
```

and fail if any forbidden crate appears:

- `tokio`
- `tokio-util`
- `hyper`
- `axum`
- `tower`
- `reqwest`
- `async-std`
- `smol`
- `rusqlite`
- `sqlx`
- `diesel`
- `sea-orm`
- `petgraph`

The dependency contract matrix must record the owning integration surface for
each franken-stack family. If an upstream feature pulls a forbidden dependency,
that feature is blocked or quarantined behind an explicit adapter gate with a
removal plan; it must not be hidden inside default features.

## Closeout Rules For Beads

A bead can close only when its verification evidence is clear.

Code or behavior beads:

- Include unit tests for new local logic.
- Include public-surface tests for CLI/MCP/hook/export/renderer/eval behavior.
- Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  relevant tests through RCH when Cargo is involved.
- Run the forbidden-dependency audit when dependencies or feature flags change.
- Record any skipped gate and why in the bead close reason or a br comment.

Docs-only strategy beads:

- May close without executable tests when they do not change code behavior.
- Must state in the close reason that no executable test is appropriate because
  the deliverable is a verification contract.
- Must name the nearest future bead or gate that will make the strategy
  executable.

Spike beads:

- Must produce a clear go/no-go recommendation.
- Must not introduce forbidden dependencies.
- Should include fixtures, contract tests, or repro notes when practical.
- Must record blocked upstream assumptions as explicit follow-up beads.

No bead should close with a vague "tested manually." The close reason must name
commands, fixtures, artifacts, or the explicit docs-only rationale.

## Initial Gate Mapping

The testing strategy makes the early readiness gates executable in this order:

1. `tests/contracts/integration_foundation.rs`
2. `tests/contracts/dependency_contract_matrix.rs`
3. `tests/contracts/sqlmodel_frankensqlite.rs`
4. `tests/contracts/asupersync_budget.rs`
5. `tests/contracts/asupersync_cancellation.rs`
6. `tests/contracts/asupersync_quiescence.rs`
7. `tests/contracts/frankensearch_local.rs`
8. `tests/contracts/cass_robot.rs`
9. `tests/golden/skeleton/*.json`
10. `tests/golden/skeleton/context_pack.md`
11. `tests/degradation_matrix.rs`
12. `tests/gates/m0_dependency_foundation.rs`
13. `tests/gates/m1_storage_status.rs`
14. `tests/gates/m2_walking_skeleton.rs`

These files do not need to exist before the skeleton bead lands, but their names
are reserved as the shared vocabulary for future work. When a later bead changes
one of these names, it must update this document, the bead description, and the
relevant golden/contract references together.

## Discovery Rules For Future Agents

Future agents should be able to find the right tests with predictable searches:

- Search a command name to find its integration and golden tests.
- Search a degradation code to find its fixture and repair assertion.
- Search a fixture ID to find seed data, expected output, and README.
- Search a schema name to find golden output and schema export tests.
- Search a bead ID in comments, fixture READMEs, or artifact manifests when a
  test exists primarily to close that bead.

Prefer names that encode behavior over implementation details. For example,
`graceful_degradation_no_cass` is better than `import_error_case_3`, and
`pack_audit_hash_stable` is better than `hash_test`.

## Gate Closure Notes

### Gate 18: Procedure Distillation Readiness (`eidetic_engine_cli-0zum`)

Gate 18 closure evidence is anchored in:

- `tests/contracts/procedure_gate18.rs`
- `tests/fixtures/golden/procedure/gate18_procedure_propose.json.golden`
- `tests/fixtures/golden/procedure/gate18_procedure_show.json.golden`
- `tests/fixtures/golden/procedure/gate18_procedure_verify.json.golden`
- `tests/fixtures/golden/procedure/gate18_procedure_export_skill_capsule.json.golden`

These fixtures and contracts are the canonical references for procedure
proposal, verification, and skill-capsule parity behavior under Gate 18.
