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

### Integration Test Targets

Cargo autodiscovery is disabled with `autotests = false`. The 512 root test
files remain in place: 481 compile as modules in five suite binaries, while
31 remain separate targets. The three existing explicit targets under
`tests/conformance/` and `tests/contracts/` also remain. This reduces the
integration link count from 515 to 39 without dropping any test files.

The suite names follow the first letter of the original file name:
`integration_a_d`, `integration_e_f`, `integration_g_m`, `integration_n_r`,
and `integration_s_z`. For a focused run, pass the original file stem as a
module filter. An exact test filter now includes that module prefix:

```bash
# Run all tests originally in tests/mesh_cache.rs.
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD --summary --no-write -- \
  cargo test --locked --test integration_g_m mesh_cache:: -- --nocapture

# List all tests in one suite before selecting an exact test name.
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD --summary --no-write -- \
  cargo test --locked --test integration_n_r -- --list
```

Standalone targets retain their original names. They include tests whose Insta
snapshot names depend on the crate name, tests with process-global tracing or
current-directory state, tests that spawn themselves with `--exact`, and
focused platform/model/hook gates. `contracts` also stays separate to preserve
its crate-relative helper imports. `proof_verify_core` includes production
source that requires its crate-root `models` module, and `failure_triage`
retains the original public helper visibility of its standalone crate.

When adding a root test file, register it in the corresponding suite or add an
explicit `[[test]]` target when process isolation is needed. The inventory tests
in `tests/suites/inventory.rs` run with `integration_a_d` and fail on omitted,
duplicate, or stale root registrations, unregistered suites, and non-compiling
module declarations hidden behind comments or suite-level conditionals.
Feature and platform conditions remain inside the original test modules.

The full gate still runs `cargo test --workspace --lib --bins --tests --examples`.
Benchmarks remain an explicit `scripts/verify.sh --include-bench` gate. Reduced
link count is a structural improvement; measured build time and full-suite
runtime must be reported separately from target discovery or `--no-run` checks.

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

Required fields for successful agent-native responses:

- `schema`: `ee.response.v2`.
- `success`: `true`.
- `data`: command-specific data, including command identity and provenance
  where relevant.
- `degraded`: structured degradation list, even if empty.

Errors use a separate `ee.error.v2` envelope with an `error` object containing:

- stable `code`
- human-readable `message`
- `severity`
- `repair` when an action exists
- structured `details`, including `recovery[]` when repair guidance is present;
  each recovery action includes its priority, kind, rationale, and safety metadata

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

### E2E Retention Mode

Agent-run e2e scripts must support an explicit retention mode for auditability
and for compliance with the repository's no-deletion operating rule.

Set these variables when an agent needs to inspect artifacts after a run:

```bash
EE_E2E_KEEP_WORKSPACE=1
EE_E2E_KEEP_ARTIFACTS=1
```

The shared `scripts/e2e_overhaul/lib/shared.sh` helper writes
`e2e_retention_manifest.json` in the e2e workspace. The manifest schema is
`ee.e2e.retention_manifest.v1` and records the epic name, workspace path, J1 log
path, `EE_BINARY`, keep flags, cleanup policy, and retained artifact paths.
When `EE_E2E_KEEP_WORKSPACE=1` is set, teardown prints the retained workspace
and manifest path to stderr and leaves the workspace in place.

E2E scripts should use the shared `epic_setup` / `_epic_teardown` path rather
than deleting workspaces directly. Any cleanup path must prove it owns the temp
workspace it is about to remove; direct recursive deletion in per-epic scripts is
not allowed. Retained workspaces and manifests are verification evidence. Do not
remove them during an agent session unless the user explicitly authorizes the
exact deletion.

### Mesh E2E Outcome Events

Mesh shell drivers may emit `stage=scheduled` records as a scenario manifest,
but scheduled records are not verification evidence. Every scheduled mesh
scenario must also emit a post-run `phase=outcome` event with `status` set to
`pass`, `fail`, or `skipped`, plus `duration_ms` and `stderr_tail`. Use
`scripts/lib/mesh_e2e_outcomes.sh` for root-level `scripts/e2e_mesh_*.sh`
drivers. The helper preserves the existing RCH-only command path while making
the JSONL log identify which scenario matrix was covered by the command result.

For command-backed mesh checks, `mesh_e2e_run_with_outcomes` is the canonical
shape: emit scheduled events, run the RCH command once, capture elapsed time and
stderr tail, then emit one outcome event per scenario. For pure static-proof
checks, emit `pass` outcomes only after all required static assertions pass; if
a preflight dependency is missing, emit `skipped` outcomes before exiting.

### Fake Tailscale Harness

SRR6.46 auto-enrollment tests must use the shared fake Tailscale harness instead
of a real tailnet in default CI. The harness lives under
`scripts/e2e_overhaul/lib/`:

- `fake_tailscale.sh` is sourced by shell e2e scripts to create deterministic
  scenario directories, rewrite `tailscale status --json` fixtures, run local
  mesh-hello responders on Unix sockets, and emit `ee.test_event.v1` JSONL.
- `fake_tailscale_shim.sh` is the PATH-prepended `tailscale` executable. It
  supports `status --json`, narrowed `status --json --self=true --peers=true`,
  and `up` logging without contacting a real Tailscale daemon.
- `test_fake_tailscale.sh` is the harness self-test and runs before later
  SRR6.46 e2e scripts in `scripts/verify.sh`.
- `tests/support/fake_tailscale.rs` mirrors the shell scenario builder for
  inline Rust integration tests without mutating process environment state.

The fake-tailnet env vars are registered in the central env registry but are
test-only: `EE_TAILSCALE_BINARY_OVERRIDE` and
`EE_TAILSCALE_PROBE_SOCKET_OVERRIDE`. Production mesh functionality must remain
strictly optional and must work as ordinary local-first `ee` when those variables
and `EE_MESH_ENABLED` are unset.

Harness scenarios must stay deterministic: fixed timestamps, lexicographic
peer order, stable node keys, no real network, and zero default latency unless a
budget test explicitly configures latency. Scenario directories and responder
socket artifacts are retained during agent sessions under the repository
no-deletion rule; use their event logs as closeout evidence rather than cleaning
them up implicitly.

### Fake OIDC IdP Harness

Team-confederation tier-2 tests use the loopback-TLS fake IdP under
`scripts/e2e_overhaul/`; they never require a provider account or outbound
network. Verification preserves three independent stages, in order:

- `fake_idp_harness_smoke.sh` covers discovery, JWKS, live device polling,
  exact RS256 verification, rotation, and the ES256 token shape.
- `fake_idp_defects_smoke.sh` proves the live server can mint adversarial
  protocol/JOSE shapes, including a signature that fails exact-input
  verification.
- `fake_idp_selfcheck.sh` is the deterministic capability/privacy/time matrix.
  It drives all oracle transitions over HTTPS, performs an actual same-state
  process restart, verifies RS256 and ES256 signatures, and scans the durable
  artifact file plus projected database/manifest/audit/log/support views.

The matrix is a provider/stimulus/reference-oracle gate, not a substitute for
production client acceptance. T7.4–T7.6 consume it for network/device,
JSON/JOSE/replay, and frame/privacy/lease enforcement respectively. Private
`/_state` output may contain ceremony secrets for test introspection; only
`/_artifact`, `/_artifact_views`, and `identity-artifact.json` are scrubbed
artifact surfaces.

The shell helper invokes curl with config, proxy, netrc, and ambient CA inputs
disabled, pins the ephemeral CA, and accepts HTTPS loopback URLs only. Logical
wall and monotonic clocks replace sleeps for poll/rollback/grace boundaries.
Ephemeral keys, certificates, ports, and ECDSA bytes vary; normalized state
transitions and assertions remain deterministic. All scenario and server state
directories are retained as evidence under the no-deletion rule.

### Opt-In Real Tailscale Smoke

`scripts/e2e_overhaul/mesh_tailscale_smoke.sh` is the only SRR6 shell driver
that may inspect a real tailnet. It is quarantined outside normal verification:
when `EE_E2E_REAL_TAILSCALE=1` is unset it exits `78` after writing a
redaction-safe `ee.test_event.v1` skip event. When enabled, it requires
`EE_REAL_TAILSCALE_PEER` to name a visible peer from `tailscale status --json`,
uses an existing `EE_BINARY`, and retains its workspace plus artifacts.

The smoke must not run Cargo, must not delete retained workspaces, and must not
emit raw node keys, Tailscale IPs, MagicDNS names, memory bodies, or remote
workspace paths. Logs use short hashes for peer and route identifiers.

### Capture Track E2E

`scripts/e2e_capture.sh` is the `bd-2vq2z.20` real-binary, no-Cargo harness
for the capture track. It emits `ee.test_event.v1` JSONL, runs under
`EE_E2E_TMPDIR=/private/tmp` in the default verifier, and retains its temporary
workspace/artifacts.

The script proves:

- `ee capture suggest` is read-only, returns `ee.capture_suggestions.v2`,
  emits explicit accept/reject commands, and preserves the source workspace in
  every returned command.
- `ee review session --propose` creates curation proposals for a fixture CASS
  session without silently storing memories.
- `ee remember --from-commit` and `--from-diff` are dry-run-first unless
  `--apply` is supplied, derive memory text from git evidence, include
  file/symbol anchors plus a BLAKE3 drift fingerprint, redact secret-like diff
  evidence, and assert the audited apply path.
- A rerun after accepted capture suppresses duplicate takeover loops instead of
  silently storing or reproposing already-covered lessons.

### Ergonomics E2E

`scripts/e2e_ergonomics.sh` is the `bd-1et0v.22` real-binary, no-Cargo
ergonomics harness. It emits `ee.test_event.v1` JSONL and retains its temporary
workspace/artifacts. The script proves:

- `ee context` and canonical `ee pack` produce the same selected pack content
  for the same workspace/query, while only `ee context` reports the
  `deprecated_alias` info degradation with an `ee pack` repair hint.
- A stale `ee` fixture earlier on PATH is reported by `ee doctor --full` as an
  advisory-only `ee_install_path` finding that names the shadowed binary and
  offline/no-network repair path.
- The PATH-shadow scenario does not change the clean doctor top-line posture;
  if upstream doctor-health work is not yet green, the script records that as a
  skipped green assertion and still enforces non-degrading parity.

### Doctor Concise E2E

`scripts/e2e_doctor_concise.sh` is the `bd-1et0v.15` real-binary, no-Cargo
doctor-health harness. It emits `ee.test_event.v1` JSONL and retains its
temporary workspace/artifacts. The script proves:

- Default `ee doctor --json` emits the compact `doctor_concise` response with
  core checks, actionable core repairs, and a one-line advisory summary.
- Default doctor output omits the mesh/RCH/verification/host-calibration
  diagnostic firehose and stays below the compact byte budget.
- `ee doctor --full --json` keeps the exhaustive checks, advisory array,
  15-row mesh auto-enrollment report, RCH worker pressure, and verification
  posture/ledger blocks.

### Doctor Health E2E

`scripts/e2e_doctor_health.sh` is the `bd-1et0v.21` real-binary, no-Cargo
doctor-health harness. It emits `ee.test_event.v1` JSONL and retains its
temporary workspace/artifacts. The script proves:

- An initialized workspace reports a green top-line posture from default
  `ee doctor --json` while keeping the default response compact and
  core-focused.
- `ee doctor --full --json` preserves the exhaustive checks, advisory blocks,
  host-calibration budget deltas, RCH worker pressure, verification posture,
  and mesh auto-enrollment diagnostics.
- Synthetic CASS-limited and RCH pressure-blocked worker probes remain
  advisory-only and do not flip the top-line doctor posture.
- `ee diag host-profile --json` stays a real-binary, side-effect-free raw host
  probe with redacted path posture; derived budget deltas are asserted through
  full doctor host calibration.

### RCH Stranded-Result Recovery

Remote verification can produce useful RCH artifacts even when the local wrapper
exits indeterminately. Use `scripts/rch_recover_verification.sh --job-id <id>
--status-json <rch-status.json> --json` to produce an `ee.rch.recovery_report.v1`
summary from `rch status --jobs --json` plus optional artifact/log hints.

The report is intentionally fail-closed:

- explicit recent RCH exit `0` is `status: pass`;
- explicit recent non-zero exit is `status: fail`;
- candidate binaries without terminal RCH status are
  `status: indeterminate_recovered_artifact`, not pass evidence;
- mismatched command hashes set `unsafe_ambiguity: true`;
- `safe_for_closure_evidence` is true only for explicit pass/fail records without
  unsafe ambiguity.

The helper never kills jobs, deletes remote files, or ingests verification
evidence. If recovery is inconclusive, use the emitted
`manual_inspection_commands` to inspect the worker and then record evidence
explicitly through the S2 verification ledger.

Before starting a Cargo gate from an agent session, run a non-mutating queue
preflight with the same path topology used for offload:

```bash
RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel/projects \
RCH_ALIAS_PROJECT_ROOT=/data/projects \
rch queue --json
```

`scripts/closeout_audit.sh --bead <id> --json` includes this queue posture in
`evidence.rch_queue_status`. Treat `stale_active_records` as a warning that
`rch exec` may hang while querying the daemon and then print
`Daemon response timed out ... running locally`. If that happens, stop the local
wrapper, do not count local Cargo output as remote verification, and record the
failed-offload caveat on the bead. The closeout audit only reports posture; it
does not restart the daemon, cancel jobs, kill workers, or delete artifacts.

RCH command surfaces differ by installed version. Some older runbooks use
manual `rch exec -- cargo ...`; the current Mac agent host may expose only the
hook/daemon command surface (`rch check`, `rch queue`, `rch status`, `rch
diagnose`, `rch hook ...`) and reject `rch exec` as an unknown subcommand. In
that state, do not run Cargo locally to compensate. Record the missing manual
execution surface as a failed-offload caveat, keep the Cargo gate unverified,
and use only non-Cargo preflight evidence (`rch check`, `rch queue --json`,
`rch status --workers --jobs`) until a supported remote or hook-routed proof is
available.

### J1 Artifact Manifests

Structured E2E logs include `kind: "artifact_manifest"` events using manifest
schema `ee.test_artifact_manifest.v1`. The bash J1 harness emits one after each
`e2e_log_command` and records the exercised binary path/hash, command hash,
source hash, execution substrate, target directory, fixture filter, J1 log path,
and retention manifest path.

The manifest is verification evidence only: it never stores raw stdout, stderr,
or domain data, and it does not upload, delete, or ingest artifacts. If a binary
hash is unavailable, `binary_hash_status` records the warning state so closeout
audits can block or caveat the evidence instead of guessing.

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
- `side_effect_class`: stable class token from the command-boundary matrix,
  such as `class=read_only`, `class=append_only`, or
  `class=derived_asset_rebuild`.
- `changed_record_ids`: durable record IDs created or updated by the step; empty
  for read-only or failed-before-mutation steps.
- `audit_ids`: audit records written by the step; empty unless the command's
  side-effect contract requires an audit entry.
- `records_rolled_back_or_audited`: changed records that were rolled back or
  explicitly audited when cancellation, budget exhaustion, storage/index
  failure, or supervised child failure happens after partial progress.
- `filesystem_artifacts_created`: side-path artifacts created by the step, such
  as backup bundles, reports, capsules, or sandbox output.
- `forbidden_filesystem_operations_checked`: true when the harness verified that
  the step did not delete files, create worktrees, silently overwrite outputs, or
  mutate outside its declared side path.
- `command_boundary_matrix_row`: reference to the command-boundary matrix row
  in `docs/mechanical-boundary-command-inventory.md`, or null if not applicable.
- `evidence_bundle_path` and `evidence_bundle_hash`: required for project-local
  skill handoffs using `ee.skill_evidence_bundle.v1`; null otherwise.
- `provenance_ids`, `trust_classes`, and
  `prompt_injection_quarantine_status`: required for skill handoffs and empty or
  `not_applicable` for purely mechanical command steps.
- `readme_workflow_row`: README or skill workflow row related to a handoff, or
  null when not applicable.
- `fixture_hashes`: map of fixture IDs to content hashes, or empty when no
  fixtures are required.
- `db_generation_before` and `db_generation_after`: DB generation numbers
  before/after command execution, or null when not applicable.
- `runtime_budget`: runtime budget in milliseconds if configured, or null when
  unbounded.
- `cancellation_status`: `not_applicable`, `not_requested`, `requested`,
  `completed`, or `timeout`.
- `cancellation_injection_point`: stable checkpoint name where cancellation or
  timeout was injected, or null when not requested.
- `observed_outcome`: deterministic runtime outcome such as `success`,
  `cancelled`, `budget_exhausted`, `storage_error`, `index_error`, or
  `supervised_child_failed`.
- `reproduction_command`: stable shell command for re-running the exact step
  from `cwd` with sanitized overrides.
- `first_failure`: null on a clean step; otherwise the shortest actionable
  diagnosis, such as `stdout_pollution`, `schema_mismatch:<schema>`,
  `missing_matrix_row:<surface>`, `unexpected_mutation`,
  `missing_fixture_hash:<fixture_id>`,
  `missing_runtime_rollback_or_audit:<outcome>`, `error.code=<code>`, or the
  first stable stderr line.

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

### Release Installer Model Smoke

Release Unix packaging lanes set `EE_INSTALL_SEMANTIC_SMOKE=require` when invoking
`install.sh`. That opt-in smoke creates a temporary workspace,
downloads the pinned `rerank-default-v1` archive, registers it through the
manifest-verifying `ee model fetch --from-file` path, stores five retrieval
candidates, and rebuilds the index. It requires `ee model status --json` to report
`data.modelLifecycle.semanticReadiness.state == "available"` and
`mode == "semantic"`. It then requires `ee search --json` to report five
model-backed results with `data.rerank.mode == "reranked"`, positive rerank
scores in the documented unit interval, per-result `scoreKind == "reranked"`,
and no `rerank_model_unavailable` degradation. This proves a default
online release can perform both the one-time Model2Vec download and native
reranker bootstrap without a stub or fusion-only false positive.

Normal `install.sh --verify` stays concise and does not download either model unless
`EE_INSTALL_SEMANTIC_SMOKE` is set. Offline or air-gapped release checks must
exercise the documented hash-fallback path separately, or use an explicitly
pre-provisioned model cache before running the same smoke. The PowerShell
installer keeps its separate Model2Vec first-use smoke; native Windows reranker
coverage runs in the five-target release-proof matrix.

### Embedding-Native Retrieval E2E

`scripts/e2e_embedding_native.sh` is the dedicated real-binary test for the
embedding-native retrieval track. The default verification runner invokes it
with `EE_E2E_TMPDIR=/private/tmp`; the script does not run Cargo and never
downloads model artifacts. If `EE_EMBED_MODEL_FIXTURE_DIR` points at a
pre-provisioned `potion-multilingual-128M` cache, the script asserts the
semantic `ee similar` path. Without that fixture it asserts the explicit
hash/lexical fallback path and records that no download was attempted.

The same run also pins remember-time `near_duplicates[]`, human duplicate
warnings, persisted embedding-dedup curation proposals, rerank posture
(`reranked` when a local reranker exists, otherwise deterministic
`fusion_only_degraded`), and eval precision metrics. Each command writes
`ee.test_event.v1` rows with stdout/stderr artifact hashes, retrieval order,
degraded codes, and the measured top-1 precision delta.

### Bundled Embeddings E2E

`scripts/e2e_bundled_embeddings.sh` is the `bd-1et0v.19` real-binary,
no-Cargo harness for proving default semantic retrieval with the bundled
model2vec backend. The default verification runner invokes it with
`EE_E2E_TMPDIR=/private/tmp`; by default it does not download model artifacts.
Set `EE_EMBED_MODEL_FIXTURE_DIR` or `EE_EMBED_MODEL_DIR` to a pre-provisioned
`potion-multilingual-128M` cache for the semantic path, or leave the cache
absent to assert the documented hash/lexical fallback.

The semantic branch mounts that real artifact into the canonical machine
registry layout (`models/model2vec/potion-multilingual-128M`), clears explicit
model-path overrides, and runs with `EE_EMBED_DOWNLOAD=off`. It therefore proves
that the verified registered artifact remains neural-local while network model
downloads and remote embedding providers are forbidden.

The `workflow_dispatch` daemon-search SLO lane is self-contained on a GitHub
Linux runner: it restores the pinned fixture from the Actions cache or
provisions it once through the public `ee model fetch embedding-default`
command, then compiles the focused test binary before entering a route-free
network namespace. The acceptance tests rerun there with Cargo offline, while
the separate 10k-document benchmark records the runner hardware alongside its
cold and warm latency measurements.

A second egress-denied branch bootstraps the same verified artifact through
`EE_EMBED_MODEL_DIR`, runs public `ee index reembed`, and verifies through
public `ee model list --json` that the row is available with matching hash,
dimension, cosine identity, and redacted source provenance. A read-only registry
inspection separately proves that the writer persisted the canonical verified
local path. It then removes the explicit model-path override and requires
search, pack, and full orient to report `neural_local`. It also drives `why-not`
candidate retrieval through the same workspace-aware embedder, rejects any
semantic-unavailable degradation, and hashes the database, WAL, and
shared-memory artifacts before and after to prove the registry lookup remains
read-only. Only the negative fixture matrix rewrites the registry row directly
through missing-source, missing source URI, unavailable, unverified,
mismatched-name, malformed-metadata, malformed-hash, mismatched-hash,
mismatched-dimension, mismatched-distance, nonlocal, symlink, and
permission-denied states. Without an explicit model-path override, each
rejected registration must execute lexical-only retrieval with
`fallbackApplied=true`, `fastScoreCount=0`, and an explicit `hash_fallback`
backend posture. A separate planted case requires a verified
`EE_EMBED_MODEL_DIR` to retain highest precedence and execute `neural_local`
despite the malformed registry row. Pack performance exposes the same
source-mode and score-coverage truth, while full orient exposes the backend and
degradation.
The harness snapshots the complete model-cache tree around every rejected
surface and runs behind both a proxy tripwire and the CI route-free network
namespace, proving that fallback neither mutates cache artifacts nor attempts
network access. A two-workspace daemon sequence queries rejected, valid, then
rejected registry state in one process so either request order must preserve
workspace-local model truth.

The harness pins the analyst paraphrase regression by storing an RBLX
bookings/FCF memory, querying a zero-ticker paraphrase, and requiring that the
RBLX memory rank ahead of unrelated analyst memories when semantic retrieval is
active. The same run asserts fresh-workspace semantic readiness, stable pack
hashes, explicit `embed_model_unavailable` fallback degradation, and
`ee eval run fx.bundled_embeddings.v1 --json` semantic-recall gain over a
deterministic hash baseline. A real download lifecycle is opt-in only via
`EE_E2E_ALLOW_REAL_DOWNLOAD=1`; that branch verifies clean JSON stdout,
downloaded model identity, and a second cached fetch.

### Native Reranker Five-Target Release Gate

The `cross-platform-determinism` CI matrix is the release gate for the pure-Rust
native reranker. It has five counted, natively hosted rows:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `aarch64-unknown-linux-gnu`
- `x86_64-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`

Every push and pull request builds and links `ee`, compiles the full unit,
contract, golden, binary, integration, and example test surface with `--no-run`,
runs the graph determinism test, and audits that target's resolved Cargo feature
tree. Each row first proves that the Frankensearch checkout matches the exact
revision in `franken-stack.lock`; the evidence sidecar records that revision.
The `x86_64-unknown-linux-musl` row remains useful extra determinism coverage but
does not count among the five release targets.

Real-model execution is an explicit, fail-closed `workflow_dispatch` profile.
The caller must enable `run_native_reranker_release_matrix` and supply
`rerank_model_url`. CI does not assume the manifest URL is live: it verifies the
downloaded archive is exactly 82,767,464 bytes with SHA-256
`adaada3ccc15ae535e9bea238d2ec05e4f39726bdcad07dd87cba9f85dc10edb`
before using it. Hosting and publishing that immutable artifact belongs to the
release bead, not to this test gate.

The strict profile runs the complete `ee` suite, the upstream
`frankensearch-rerank` suite with `--no-default-features --features native`, and
`scripts/e2e_native_reranker.sh` against the built binary on every target. The
upstream model files are staged at its required real-model fixture path so those
tests cannot silently count a missing model as green; the gate also rejects any
upstream real-model skip diagnostic. The E2E harness emits an
`ee.rerank_determinism.vector.v1` artifact per target. The aggregate job requires
exactly five unique target artifacts, exact query/fusion/reranked ordering, and
reranker scores within `0.01` of the x86_64 Linux reference. Per-command UTC
start/completion/status logs and all available per-target artifacts are retained
when a target fails; the aggregate job then fails without issuing a passing
verdict.

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

### Graph Test Determinism

Graph feature tests use the same determinism contract as context-pack and
runtime tests, with two graph-specific constraints:

- Any graph test that needs runtime scheduling uses `LabRuntime` through the
  shared `ee::testing` helpers, not wall-clock sleeps or an ambient executor.
- Any graph test that needs semantic seed vectors uses `HashEmbedder` or a
  checked-in fixture. It must not instantiate real `model2vec` or `fastembed`
  backends in CI.

Tests that deliberately exercise a real embedding backend must be feature-gated
outside the default CI profile and named as non-CI coverage in their module
docs. The default policy sentinel is `#[ignore]` and requires
`EE_RUN_NON_CI_REAL_EMBEDDER_GRAPH_TEST=1` before any non-CI real-backend graph
coverage is allowed to run. Graph test fixtures should make this visible in
source rather than relying on local machine configuration.

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

Support-bundle replay evidence is covered by the `pack_replay_summary.json`
section. That artifact records pack IDs, pack hashes, ledger hashes, freshness
state counts, degradation codes, redaction classes, and derived-asset metadata,
but it deliberately hashes query text and omits raw memory content, `why` text,
and full ledger payloads. Tests must keep that summary parseable as
`ee.support_bundle.pack_replay_summary.v2` and prove secret-like query or
provenance text does not appear in the bundle.

Support-bundle regression-causality evidence is covered by
`regression_causality_summary.json`. That artifact records the
`ee.support_bundle.regression_causality_summary.v1` schema, points back to
`ee.regression_causality.v1`, keeps normalized evidence rows and ranked
non-authoritative hypotheses, and records redaction posture. Tests must prove it
omits raw logs, raw mail bodies, raw memory bodies, private paths, and copied
input artifacts while retaining hash/provenance-based evidence for handoff and
support triage. Operator-facing closeouts should follow
[`docs/agent-ux/regression-causality.md`](agent-ux/regression-causality.md):
record the capsule hash, inspected evidence refs, read-only follow-up command
hashes, outcome status, and tracker posture, and never close a source bead from
an abstaining or remote-source-unknown capsule.

## Replay, Freshness, and Egress Tests

Pack replay and evidence freshness testing verifies that:

1. **Ledger persistence**: `ee context` and `ee pack` persist deterministic
   selection ledgers to `pack_records.ledger_json`.
2. **Ledger reconstruction**: `ee pack replay` reconstructs the selection
   explanation from stored ledgers without re-running retrieval.
3. **Freshness detection**: Provenance freshness states (`fresh`,
   `missing_source`, `changed_source`, `unreachable_source`,
   `unsupported_source`, `unknown`) are threaded into pack and why responses.
4. **Egress safety**: Public outputs (context, search, why, pack, support
   bundles) do not leak raw secret-like spans after redaction.

### Test Files

| File | Coverage |
| --- | --- |
| `tests/e2e_pack_determinism.rs` | Ledger persistence, query-file paths |
| `tests/no_mocks_e2e.rs` | Real-binary pack with ledger assertions |
| `tests/redaction_egress_no_mock.rs` | Egress matrix for secret-like spans |
| `tests/freshness_contracts.rs` | Freshness states, deterministic ordering |
| `tests/degraded_honesty.rs` | Support bundle schema validation |
| `tests/support_bundle_perf_compare.rs` | Bundle profile/perf evidence |
| `tests/json_contract_snapshots.rs` | Stable pack ledger JSON contracts |

### Ledger Contract Tests

Ledger tests must verify:

- Empty packs have valid empty-ledger structure
- Lexical-only degradation produces stable ledger
- Graph-unavailable degradation produces stable ledger
- Redacted items have redaction classes in ledger
- Deterministic tie ordering under identical scores
- Ledger hash matches normalized payload hash
- Old pack records without ledgers report `pack_replay_ledger_missing`; malformed
  and integrity-mismatched ledgers report their distinct catalogued codes

### Freshness Contract Tests

Freshness tests must verify:

- `changed_source` when file content differs from stored hash
- `missing_source` when referenced file no longer exists
- `unsupported_source` for non-verifiable provenance schemes
- Mixed freshness states are sorted deterministically
- Freshness states appear in pack/why JSON without diagnostics on stdout
- Repair hints are actionable (`ee remember --update --source <path>`)

### Egress Matrix Tests

Egress matrix tests (ADR 0025 verification obligation) must verify:

- Context pack JSON stdout contains redaction placeholders, not raw secrets
- Search result JSON does not expose raw secret-like memory content
- Why response JSON does not expose raw secret-like provenance
- Support bundle `pack_replay_summary.json` excludes query text and memory
  content
- stderr diagnostics do not include raw secret spans
- Command JSONL logs redact secret-like input before storage

Run targeted coverage:

```bash
rch exec -- cargo test --test e2e_pack_determinism
rch exec -- cargo test --test redaction_egress_no_mock
rch exec -- cargo test --test freshness_contracts
rch exec -- cargo test --test degraded_honesty pack_replay
```

See `docs/pack-replay.md` for user-facing documentation.

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

The five-target native-reranker release gate repeats this audit against each
target-resolved feature tree and additionally rejects `ort`, `ort-sys`,
`onnxruntime`, and `onnxruntime-sys`. Scanning a lockfile is insufficient because
it may contain an inactive optional backend; the target-resolved tree is the
release evidence.

## Closeout Rules For Beads

A bead can close only when its verification evidence is clear.

Code or behavior beads:

- Include unit tests for new local logic.
- Include public-surface tests for CLI/MCP/hook/export/renderer/eval behavior.
- For shell E2E scripts that emit `ee.test_event.v1`, run the E2E event
  contract radar described in `docs/e2e-event-contract-radar.md` and record any
  `advisory_gap`, `known_gap`, or `fail` posture in the close reason.
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

### Central Verification Runner

All readiness gates are orchestrated by `scripts/verify.sh`. This is the single
entry point for verifying the codebase:

```bash
./scripts/verify.sh
```

The script runs gates in order (forbidden-deps → cargo test → E2E suites),
reports per-gate exit codes and durations, and lists artifact directories
written by the E2E harnesses. See `./scripts/verify.sh --help` for details.

Hosted GitHub Actions is split on purpose (`bd-o7wh0`). `.github/workflows/ci.yml`,
`release.yml`, and `macos-ee-artifact.yml` remain `disabled_manually`; enabling
`ci.yml` on `push: main` during a swarm would queue 90-minute cargo shards
against a predicted-red tree. The hosted executing gate for cargo-free
invariants is `.github/workflows/ci-static.yml` (`CI Static`): forbidden-deps,
migration-registry, closure-lint, vision-coverage, MCP lib-test vacuity guard,
contract-drift-radar, and `cargo fmt --check`. A green `CI Static` run is not
"CI restored." Cargo tests and clippy stay on the pinned RCH lane
(`scripts/rch_verify.sh`). Dispatch a `CI Static` scratch run when you need a
hosted proof that will not be cancelled by the next push to `main`.

## Every Gate Has A Population, And Green Only Covers That Population

A green gate answers one question: *did everything in this gate's population
pass?* It does not answer *is the tree clean?* The two read identically in CI and
come apart whenever the population is smaller than the thing a reader assumes it
covers. The count of consecutive green runs says nothing about **which
population** was green.

Five gates were examined on 2026-09-21 and each turned out to under-cover in a
different way. None was broken; each had a population narrower than its name
suggests.

| gate | population is actually | how it came apart |
|---|---|---|
| `e2e_invocation_audit.sh` | files ripgrep will search | ripgrep applies `.gitignore` only inside a repo; without `.git` it searched gitignored-but-synced files and counted *prose naming a script* as invoking it — 52 orphans vs 6 from the same commit. Fixed with `rg --no-require-git` (`a52fabd40`). |
| `scripts/lib/mod_reachability.py` | what `git ls-files` returns | outside a repo it exits 128 with empty stdout; an unchecked return code made the population 0, so nothing was unreachable and the gate reported clean. Now raises (`a52fabd40`). |
| `cargo clippy -- -D warnings` | whatever actually runs it | prescribed by AGENTS.md and run by no enabled workflow, so the lane accumulates and sheds reds unobserved and a measurement taken against it decays silently (`bd-clippy-gate-has-no-runner-186zj`). |
| `cargo test` shards | workflows that are enabled | the shards exist in `ci.yml`, which is `disabled_manually`. A gate that exists in a disabled workflow is indistinguishable from one that passes. |
| `cargo fmt --check` | **module-reachable files only** | rustfmt walks `mod` and `#[path]`; it never follows `include!`. `src/cass/backfill_public_tests.rs` is reached only by `include!` from `src/cass/import.rs:2435`, so it compiled and carried 329 lines of rustfmt diff behind consecutive green runs (`bd-39y21`). |

The last one is the sharpest illustration, because nothing was hidden. The commit
that introduced it (`6a425cf69`) states the consequence in the diff — "rustfmt
does not follow `include!`, so the file compiles but stays unformatted, which the
reachability gate reports on its own non-failing line" — and
`mod_reachability.py` prints that blind spot in its own output on every run. The
information was carried correctly in two places and still nobody acted on it,
because neither place is where a reader looks when CI is green.

Note also why the obvious remedy does not apply there: that file is a *fragment*,
not a module. It uses `TestResult`, `unique_test_dir` and `write_fake_cass`
unqualified and has no `use super::*`, so a `#[path] mod` would not inherit the
scope it needs. `include!` is the mechanism it was authored for. Widening
rustfmt's population — not re-declaring the file — is the tractable direction.

**Rules that follow:**

- State a gate's population next to its name, in the gate's own output or a
  comment at its call site. "All root scratchpad patterns matched" is a better
  pass line than "OK" because it names what was checked.
- When a gate cannot run, say so with a distinct exit code rather than passing or
  failing. `e2e_repo_hygiene.sh` exits 3 for "no git repository" precisely so it
  is not read as a hygiene verdict.
- Before citing a green run as evidence on a bead, write down the population it
  covers. If that sentence is hard to write, the citation is weaker than it looks.
- When a gate reports a caveat on a non-failing line, that caveat is a finding
  with no owner. File it.

### Declare a contract, not a list

The population defects above are the runtime half of one problem. The other half
is descriptive: prose and config that *enumerate* what is covered.

**A list must be maintained by whoever changes the thing it describes. A contract
is a property that either holds or does not, and its check can find its own
population.** That difference decides whether a description can go stale.

The failure mode is counterintuitive, which is why careful people keep producing
it: **prose describing a set of checks goes stale precisely when someone ADDS a
check.** The moment of improvement is the moment of drift. Four instances landed
in this repo on 2026-09-21 alone, every one created by doing the right thing
somewhere else — a deadline comment, a memory note, a `:1607` line citation that
had moved to `:1923`, and the property list in `scripts/verify.sh` that said the
feature set was unchecked one commit *after* the check landed, then said the
target triple was unchecked one commit after that.

A list is not always wrong. It is wrong **unchecked**. Three workable shapes, in
order of preference:

1. **A contract the check can evaluate.** `verify-budget.toml`'s
   `[candidate_binary]` names which binary a run verifies and a test asserts
   verify.sh's resolution agrees; neither side is a copy of the other. The
   evidence-outputs clause (`bd-ovsjv`) wants this shape too — "every stage
   emitting evidence prints an `Artifacts:` line, and here are the roots" is a
   property; a list of paths is a second copy of something discovered at runtime.
2. **A list with a derived cross-check**, where the test computes BOTH sides from
   their real sources and diffs them.
   `every_default_feature_is_reported_by_build_features` parses `default = [...]`
   from `Cargo.toml` and `BuildFeature::new(` from `src/core/mod.rs`; it holds
   because it copies neither. Note what follows: do **not** restate that feature
   list in a third place, because a third copy is a third seat for drift and the
   cross-check only guards two.
3. **A list that states its own invariant**, as a last resort when nothing can
   check it. Enumerate rather than generalise — an enumeration is falsifiable and
   a generalisation is not — and say in the text that it must change in the same
   commit as the thing it describes.

If you find yourself writing "this checks X, Y and Z" in a comment, ask whether a
test could derive X, Y and Z instead. If it could, the comment is already debt.

### A constraint you impose on yourself has a population too

The sections above are about distrusting what a gate claims to cover. The same
scepticism is owed to your own sense of what a tool can do, and it is harder to
apply there for a structural reason: **a gate's coverage claim is external and
checkable, while your own belief about a limitation feels like knowledge rather
than a claim.** Nothing will ever contradict it, because nothing is testing it.

Worked example from 2026-09-21. A `run_stage` defect (`bd-ovsjv`) sat unfixed for
two hours behind the stated bound "I cannot run `verify.sh` end to end" — RCH was
hitting its build cap and the clean-overlay lane is cargo-only, both true. The
bound was true of the FULL SWEEP and false of what the work actually needed:
`./scripts/verify.sh --plan-doc-smoke` runs exactly one stage through `run_stage`
and exits with the normal banner, and there are two more narrow modes beside it
(`--fuzz-target-audit-self-test`, `--ci-smoke`). Every measurement that finally
closed the bead came from the mode nobody had looked for — including a SECOND
defect that reading had never revealed, that the artifact index is unreachable on
a hard-failing run because `run_stage` exits hundreds of lines before the
end-of-file printer.

**The check, before accepting that something cannot be verified:** write the
bound down as a sentence, then ask what its population is. "I cannot run X" is
almost never true of all of X — read `--help`, list the subcommands, look for the
narrow mode. A capability boundary inferred from one refusal, or from the most
expensive path, is a measurement with a date like any other
(`reference_rch_exec_cargo_only_e2e_unrunnable` records the same class costing
this repo months of e2e beads closed "by inspection" against a limit that had an
official escape hatch the whole time).

The tell is that the bound is doing work for you: it explains why the hard thing
can be deferred. That is exactly when it deserves the scrutiny you would give a
gate reporting green.

### A guard is not deployed until the caller that needs it calls it

Writing the guard is the part that feels like the work. It is the smaller half.
**The question that decides whether a guard exists in practice is: which callers
route through it, and is the one that needed it most among them?**

Two instances landed on 2026-09-21, both in guards written that same day:

- `scripts/check-include-fmt.sh:61` skipped a listed-but-missing path with
  `[ -f "$rel" ] || continue` — inside the gate whose entire purpose is that a
  narrowed population must not read as a pass. The guard did not apply its own
  rule to itself.
- `scripts/lib/ee_binary_resolution.sh::ee_binary_executes_here` answers "can
  this binary run on this host", and **24 scripts source it, including
  `scripts/verify.sh`**. Neither RCH harness did — and the pinned lane is
  precisely where a wrong-platform artifact lands, since an RCH run can exit 0
  having written a Linux ELF over a macOS target dir. The result was a run that
  died with `Exec format error` and no name for it.

Both are the population class pointed at a *guard's callers* rather than at its
inputs: the guard is correct, its coverage is narrower than its reputation, and
nothing reports the gap because a non-caller emits no signal at all.

**Two checks, cheap, at the moment you write a guard:**

1. `grep -rl <guard-symbol> scripts/ src/` and read the list as a population.
   Then ask which callers are *missing* and why — an absent caller is invisible
   in every other view.
2. Ask where the failure this guard names actually occurs. If that location is
   not in the list from (1), the guard is not deployed where it matters, however
   many other callers it has.

A related tell, worth its own line: **a guard written as "did it print anything"
cannot distinguish a result from an error message.** `candidate_ee_bin` accepted
any non-empty `--version` output, and a foreign binary answered `exec format
error: <path>` — non-empty, therefore accepted. Key on exit status (126/127 mean
"could not execute"), not on output volume.

### Two different blindnesses: the population, and the predicate

Everything above this section is one failure: **the gate looked at the wrong
set.** Ripgrep's filtering changing with the tree, `git ls-files` collapsing to
zero, clippy with no runner, shards in a disabled workflow, rustfmt's module
reachability, an artifact index keyed on success, a `--dry-run` probe that cannot
reach three of five guards, a guard whose callers omit the one that needed it.
All population.

`candidate_ee_bin` is a different animal and worth separating, because the
remedies differ. **It looked at exactly the right thing and asked it the wrong
question.** The population was perfect — one binary, the correct one, probed
directly — and the predicate could not tell success from failure. A guard like
that is blind *over a perfect population*, so every population fix in this
document would have left it broken.

| blindness | question to ask | how it hides |
|---|---|---|
| **population** | *what did this actually examine?* | the missing members emit no signal |
| **predicate** | *could this answer have come from a failure?* | the answer looks like data |

The predicate check is the cheaper of the two and almost nobody runs it: **write
down the output your guard would produce on a FAILING input, push it through your
own condition, and confirm it comes out false.** `-x` on a Linux ELF under macOS
is true; `--version` on it prints a non-empty string. Both inputs are failures
that satisfy the predicate, and one minute with either would have shown it.

Prefer predicates over proxies wherever a real one exists: an exit status over
output volume, an explicit `[ -d ]` over a path prefix, a parsed value over a
substring match. A proxy is a predicate you have not checked the failure case of
yet.

### The third blindness: a field that answers no question at all

The two blindnesses above both assume the guard *asks something*. A third shape
skips the question. It is the one this repo actually accumulated, so it gets its
own section and a census.

**A field whose name is a question must have a line that answers it. If you
cannot point at that line, the field is decoration** — and decoration shaped like
`success` is worse than no field, because it is the field a consumer reaches for
first.

Five landed instances, ordered by how hard each was to *see* rather than to fix:

| # | shape | the defect | fix |
|---|---|---|---|
| 1 | **the proxy** | the predicate is real but measures a stand-in — `--version` printing a non-empty string, where a Linux ELF's `exec format error` is non-empty too | `4397160ab` |
| 2 | **the collapsed range** | the predicate is right and its *type* cannot carry the answer — five terminal statuses folded into exit 1 | `aefc006ef` |
| 3 | **the overloaded absence** | one `null` standing for three distinct states: not attempted, attempted-and-empty, deliberately bypassed | `a527f4f27` |
| 4 | **the wrong question** | the field is computed, from a predicate about a *different subject* — "did the wrapper execute its own logic" inside a receipt reporting whether verification happened | `923a2a4c8` |
| 5 | **the literal** | the field is a constant. No question is asked at all | `4f474df02` |

The ordering is the point. Going down the table the fix gets **easier** and the
defect gets **harder to notice in review**, because there is progressively less
wrongness on the page to catch the eye. At the bottom, `"success": True` sitting
in a payload has no wrong question visible — there is no question — and a
reviewer's eye supplies the justification the code never gave. Instance 5 sat two
lines above a `status` that already had four values, one of them `healthy`, and
survived a survey that was *specifically hunting this class*.

**Four of the five were in `scripts/rch_verify.sh`.** Not because that file is
badly written, but because it is the repo's proof emitter: it is where fields
named `success`, `status` and `verdict` are *supposed* to live, so it is where a
dishonest one is camouflaged by a hundred honest ones. Density follows the
vocabulary. When auditing for this class, go to the file that legitimately speaks
the language.

#### The check

Cheap, and it finds all five shapes:

> For each success-shaped field, name the expression that produces it. Then ask
> whether that expression's **range** has as many distinct values as the subject
> has outcomes.

- a constant has range 1 — instance 5
- `exit 1` has range 2 against five statuses — instance 2
- `null` has range 1 against three states — instance 3
- range is fine but the *subject* is wrong — instance 4
- range and subject fine but the measurement is a stand-in — instance 1

#### Three states, not two

A pass and an abstention must not share an exit code or a status word
(`c9f49b736`). A dry run that never executed has no success verdict, and
emitting `true` there is this same defect pointed the other way. `923a2a4c8`
carries the precedent: `exit_code null` + caller-declined → `success: null`,
`verdict: "abstained"`. An argument *refusal* also never executed, but it is a
refusal rather than an abstention and keeps `false`.

The corollary bites when one schema has two producers: if producer A carries
`success` and producer B omits it, a consumer that learned `.success` from A
reads missing-and-falsy from B, which is this class inverted — **absence
indistinguishable from failure**. Tracked for `ee.rch.worker_root_canary.v1` at
`bd-ldypi`.

#### Do not hunt the literal

The obvious sweep is the wrong sweep, and it costs real time to rediscover that.
Measured on this tree: 440 `"success": true` literals → 295 on production paths
→ 94 beside a computed failure-bearing sibling → **0 real**, because the
`ee.response.v2` envelope contract makes the literal *correct* almost everywhere.
`success` there describes whether the command completed and produced a
well-formed response; partial failure belongs in `degraded[]`.

So hunt the emitters instead: **find the functions that take a success-shaped
argument, then read what their call sites pass alongside a non-zero exit.** In
this repo that shape is rare — five Rust functions — which is itself the warning.
The danger is not volume. It is that one such emitter feeds every receipt a lane
reads, and thirteen call sites passed `true` beside `exit_code 1` through exactly
one of them.

One caveat on resolvers built for this hunt: **a fixed-line window is the defect,
not its size.** Resolve by structure — brace-match the literal and record the
key's nesting depth. A key at depth 1 describes *the command*; nested deeper it
describes *the subject*, and `data.status = "mismatch"` from a replay that ran
fine is a successful command reporting a mismatch. Lexically identical to the
defect, structurally its opposite, and no window of any size separates them.

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

## Verification Drift Guard (`eidetic_engine_cli-eism`)

The drift guard prevents "invisible baseline drift" where failing verification
gates become normalized background noise without explicit tracking.

### Rule

When a verification gate is red (failing), there MUST be an open bead tracking
that failure. If a gate fails and no corresponding bead exists, the drift guard
fails with exit code 1.

### Implementation

The guard lives at `scripts/verification-drift-guard.sh` and runs as Gate 2.5 in
`verify.sh`, immediately after the closure linter. It checks:

1. **Closure-lint violations**: If `.closure-lint-report.json` shows violations,
   there must be an open bead with keywords matching "closure" or "lint" in its
   title, labels, or description.

2. **Test failures**: If vision coverage shows significant missing surfaces,
   there must be an open bead tracking core functionality gaps.

3. **Forbidden dependencies** (on-demand): If `cargo tree` finds forbidden deps,
   there must be an open bead tracking the violation.

### How Agents Should File Blockers

When a verification gate fails, create a bead before closing related work:

```bash
br create --title "[verify] Fix <gate-name> violations" \
    --type bug --priority 1 \
    --description "Gate <name> is red. <N> issues found. Details: <summary>"
```

Include keywords that the drift guard can match: "closure", "lint", "test",
"forbidden", "verification", or the gate name itself.

### Contract Tests

`tests/verification_drift_guard.rs` provides contract tests for the guard:

- Script exists and is executable
- `--json` produces valid JSON report
- `--help` flag works
- `verify.sh` includes the drift guard gate

### Accepted Exclusions

None. All red gates must have tracking beads. If a gate is intentionally ignored
(e.g., feature-gated or out of scope), the gate itself should be disabled rather
than carrying silent failures.
