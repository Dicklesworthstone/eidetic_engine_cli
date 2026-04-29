# Closure Dossier Contract

This document defines `closure_dossier.v1`, the standard evidence package for
closing beads in `ee`.

The closeout rule is simple: another agent must be able to understand what
changed, what was verified, which artifacts prove it, and what remains out of
scope without rereading the original plan or trusting a vague "tests pass"
claim.

## Contract Names

| Contract | Schema Name | Purpose |
| --- | --- | --- |
| Closure dossier | `closure_dossier.v1` | Structured closeout evidence for a bead, gate, or epic. |
| Closure validation summary | `ee.closure_dossier.validation.v1` | Future machine-readable output for dossier validation. |

## Profiles

| Profile | Applies To | Evidence Weight |
| --- | --- | --- |
| `docs_light` | Docs-only contracts, ADRs, planning notes, design-only spikes | Human-readable summary, changed docs, docs checks, explicit no-executable-test rationale, nearest executable follow-ups. |
| `implementation_full` | Rust code, public CLI behavior, storage, search, pack, policy, output, adapters | Full command/test/artifact/redaction/degradation/effect evidence. |
| `fixture_contract` | Golden/schema/e2e/replay/evaluation fixture work | Fixture IDs, scenario IDs, manifest hash, schema/golden status, artifact paths, first-failure behavior. |
| `readiness_gate` | Gate beads and milestone evidence | Gate criteria, exact checks, pass/fail/skipped cases, artifact links, remaining blockers. |
| `epic_rollup` | Epic or multi-bead closure | Child bead list, aggregate evidence, skipped/deferred work, risks, release readiness. |

Trivial docs work should use `docs_light`; safety-critical docs that define a
future executable contract may add selected `fixture_contract` fields.

## Required Fields

Every dossier has these fields:

| Field | Required | Meaning |
| --- | --- | --- |
| `schema` | yes | Must be `closure_dossier.v1`. |
| `bead_id` | yes | Bead ID such as `eidetic_engine_cli-hkk7`. |
| `bead_title` | yes | Bead title at close time. |
| `profile` | yes | One of the profiles above. |
| `closed_by` | yes | Agent name and program. |
| `closed_at` | yes | RFC 3339 timestamp. |
| `implementation_summary` | yes | Concrete summary of changed behavior or docs. |
| `files_changed` | yes | Paths grouped by `rust`, `tests`, `fixtures`, `docs`, `config`, `generated`, or `other`. |
| `public_surfaces_affected` | yes | Commands, schemas, hooks, MCP tools, exports, renderers, or `none`. |
| `verification` | yes | Commands/checks run with status and notes. |
| `stdout_stderr_status` | yes | How stdout data purity and stderr diagnostics were verified, or why not applicable. |
| `redaction_status` | yes | Redaction fixture status, not applicable reason, or skipped rationale. |
| `degradation_coverage` | yes | Degraded/offline cases covered or skipped with rationale. |
| `effect_expectation` | yes | Expected command effect class and observed mutation evidence. |
| `artifacts` | yes | Log, golden, replay, fixture, or generated artifact paths, or explicit none. |
| `skipped_checks` | yes | Any required checks not run, each with a reason and follow-up bead where possible. |
| `follow_up_bead_ids` | yes | Follow-up beads for deferred coverage, risk, or implementation. |
| `close_reason_summary` | yes | Short text suitable for `br close --reason`. |

Optional fields:

| Field | Use When |
| --- | --- |
| `fixture_ids` | Fixture, e2e, golden, replay, or eval evidence exists. |
| `scenario_ids` | North-star scenario evidence exists. |
| `manifest_hashes` | Fixture or command-effect manifests were used. |
| `schema_names` | Public machine contracts changed or were validated. |
| `golden_paths` | Golden files were added, updated, or checked. |
| `replay_paths` | Failure replay or repro artifacts were produced. |
| `dependency_audit` | Dependencies, feature flags, or runtime surfaces changed. |
| `risk_notes` | Residual risks remain after closure. |

## Verification Entry

Each verification item records:

| Field | Meaning |
| --- | --- |
| `kind` | `format`, `clippy`, `test`, `golden`, `schema`, `e2e`, `dependency_audit`, `docs_check`, `manual_inspection`, or `not_applicable`. |
| `command` | Exact command run, or `none` for manual/doc checks. |
| `offloaded` | `true` when CPU-heavy Cargo/test work used RCH. |
| `status` | `passed`, `failed`, `skipped`, or `not_applicable`. |
| `artifact_paths` | Files or directories containing logs/output. |
| `notes` | Concise result or rationale. |

Cargo, test, clippy, and build commands must be run through RCH. A dossier that
records a CPU-heavy command with `offloaded = false` is invalid unless it also
records a user-approved exception.

## Artifact Policy

Generated artifacts stay out of commits unless they are intentional golden
fixtures, schemas, scripts, source fixtures, or tracked docs. Dossiers may point
to generated paths under `target/`, but they should not require those paths to be
committed.

Every generated artifact reference should include enough context to interpret it:

- command argv
- cwd and resolved workspace
- sanitized environment overrides
- elapsed time
- exit code
- stdout/stderr artifact paths
- schema/golden validation status
- redaction status
- expected and observed effect class
- degradation codes observed
- first-failure diagnosis when failing
- `fixture_manifest.v1` hash when fixture-backed

## Closeout Checklist

Before closing a bead, the agent checks:

1. The bead status and dependencies match the work actually completed.
2. The changed files are scoped to the bead and do not include unrelated user or
   agent changes.
3. Public output changes name the affected schema and golden/contract coverage.
4. Rust/Cargo changes were checked with RCH-backed `cargo fmt --check`,
   `cargo clippy --all-targets -- -D warnings`, and the relevant tests.
5. Dependency or feature changes ran the forbidden-dependency audit.
6. Read-only and dry-run claims have mutation evidence or are tied to a future
   command-effect manifest check.
7. Redaction-sensitive changes include redaction evidence or a clear skip reason.
8. Degraded/offline branches are tested or explicitly deferred to named beads.
9. Generated artifacts are either ignored under `target/` or intentionally
   tracked as fixtures/goldens/docs.
10. The `br close --reason` text includes the dossier summary or points to a
    tracked dossier path.

## Invalid Close Reasons

These are invalid because they are not evidence:

- "tests pass"
- "done"
- "implemented"
- "manual testing ok"
- "should work"
- "fixed the issue"

Valid close reasons name the artifact or check:

- "Added `docs/testing-strategy.md`; docs-only strategy bead, no executable test
  appropriate; nearest executable follow-up is `eidetic_engine_cli-57k1`."
- "Implemented `ee status --json`; RCH-backed fmt/clippy/test passed; smoke e2e
  validated stdout-only JSON and unknown-command stderr behavior."
- "Added CASS contract fixtures; validator artifacts are under
  `target/ee-e2e/cass_contract/<run-id>/`; `external_adapter_schema_mismatch`
  golden covers unknown schema versions."

## Example: Docs-Only Bead

```toml
schema = "closure_dossier.v1"
bead_id = "eidetic_engine_cli-8o2v"
bead_title = "EE-TST-001: Define comprehensive testing and logging strategy"
profile = "docs_light"
closed_by = "SwiftBasin / codex-cli"
closed_at = "2026-04-29T15:17:27Z"
implementation_summary = "Added the project testing and logging strategy."
public_surfaces_affected = ["none"]
stdout_stderr_status = "not_applicable: no command behavior changed"
redaction_status = "not_applicable: docs-only contract"
degradation_coverage = "staged in degradation matrix"
effect_expectation = "read_only docs change"
artifacts = []
follow_up_bead_ids = [
  "eidetic_engine_cli-57k1",
  "eidetic_engine_cli-4wj5",
  "eidetic_engine_cli-z6kq"
]
close_reason_summary = "Docs-only strategy contract; no executable test appropriate until the logged e2e and golden/schema runner beads land."

[[files_changed]]
category = "docs"
paths = ["docs/testing-strategy.md"]

[[verification]]
kind = "docs_check"
command = "git diff --check -- docs/testing-strategy.md"
offloaded = false
status = "passed"
artifact_paths = []
notes = "Whitespace check passed."

[[skipped_checks]]
kind = "cargo"
reason = "No Rust/Cargo files changed."
follow_up_bead_ids = []
```

## Example: CLI Feature Bead

```toml
schema = "closure_dossier.v1"
bead_id = "eidetic_engine_cli-84l"
bead_title = "EE-005: Add CLI parser and global flags"
profile = "implementation_full"
closed_by = "AgentName / codex-cli"
closed_at = "2026-04-29T00:00:00Z"
implementation_summary = "Added global flag parsing and stable usage errors."
public_surfaces_affected = ["ee --help", "ee --version", "ee status --json"]
schema_names = ["ee.response.v1", "ee.error.v1"]
stdout_stderr_status = "smoke e2e asserts JSON stdout is clean and usage errors write diagnostics to stderr"
redaction_status = "not_applicable: no memory or secret-bearing output"
degradation_coverage = "not_applicable for parser-only slice"
effect_expectation = "read_only"
artifacts = ["target/ee-e2e/cli_globals/<run-id>/"]
follow_up_bead_ids = ["eidetic_engine_cli-jyx", "eidetic_engine_cli-uzz"]
close_reason_summary = "CLI global flags implemented with RCH-backed fmt/clippy/test and smoke e2e artifacts."

[[files_changed]]
category = "rust"
paths = ["src/cli/mod.rs", "src/models/mod.rs", "tests/smoke.rs"]

[[verification]]
kind = "format"
command = "rch exec -- cargo fmt --check"
offloaded = true
status = "passed"
artifact_paths = []
notes = ""

[[verification]]
kind = "clippy"
command = "rch exec -- cargo clippy --all-targets -- -D warnings"
offloaded = true
status = "passed"
artifact_paths = []
notes = ""

[[verification]]
kind = "test"
command = "rch exec -- cargo test --all-targets"
offloaded = true
status = "passed"
artifact_paths = ["target/ee-e2e/cli_globals/<run-id>/"]
notes = "Smoke tests cover stdout/stderr separation."
```

## Example: Storage Or Indexing Bead

```toml
schema = "closure_dossier.v1"
bead_id = "eidetic_engine_cli-q9f"
bead_title = "EE-040: Wire SQLModel FrankenSQLite connection"
profile = "implementation_full"
closed_by = "AgentName / codex-cli"
closed_at = "2026-04-29T00:00:00Z"
implementation_summary = "Wired the repository connection through SQLModel and FrankenSQLite."
public_surfaces_affected = ["ee status --json", "ee init --json"]
schema_names = ["ee.response.v1", "ee.status.v1"]
stdout_stderr_status = "status/init e2e validates stdout JSON only; migration diagnostics go to stderr artifacts"
redaction_status = "paths are redacted to workspace/data path classes"
degradation_coverage = "migration_required and storage_unavailable goldens"
effect_expectation = "audited_write for init; read_only for status"
artifacts = ["target/ee-e2e/storage_connection/<run-id>/"]
fixture_ids = ["fx.fresh_workspace.v1", "fx.migration_required.v1"]
scenario_ids = ["usr_workspace_continuity"]
manifest_hashes = ["fixture_manifest.v1:blake3:<hex>"]
follow_up_bead_ids = ["eidetic_engine_cli-koat", "eidetic_engine_cli-6dxy"]
close_reason_summary = "Storage connection implemented with migration/degradation fixtures and RCH-backed verification."

[[verification]]
kind = "dependency_audit"
command = "rch exec -- cargo tree -e features"
offloaded = true
status = "passed"
artifact_paths = ["target/ee-e2e/storage_connection/<run-id>/dependency-tree.txt"]
notes = "Forbidden dependency grep found no Tokio, rusqlite, SQLx, Diesel, SeaORM, or petgraph."
```

## Example: Readiness Gate Or Epic

```toml
schema = "closure_dossier.v1"
bead_id = "eidetic_engine_cli-s67f"
bead_title = "Gate 6: CASS Robot Contract Fixture"
profile = "readiness_gate"
closed_by = "AgentName / codex-cli"
closed_at = "2026-04-29T00:00:00Z"
implementation_summary = "Gate 6 passed for CASS robot JSON compatibility."
public_surfaces_affected = ["ee import cass --dry-run --json"]
fixture_ids = ["fx.cass_v1.v1"]
schema_names = ["cass.api-version", "cass.capabilities", "cass.search", "cass.view"]
stdout_stderr_status = "CASS subprocess stdout parsed as JSON; stderr captured as diagnostic artifact"
redaction_status = "fixture source documented as scrubbed synthetic session data"
degradation_coverage = "external_adapter_schema_mismatch golden covers unknown CASS schema version"
effect_expectation = "dry_run"
artifacts = ["target/ee-e2e/cass_robot_contract/<run-id>/"]
follow_up_bead_ids = ["eidetic_engine_cli-c48h"]
close_reason_summary = "Gate 6 passed with fixture-backed CASS robot contract evidence."

[[verification]]
kind = "schema"
command = "rch exec -- cargo test --test cass_robot_contract"
offloaded = true
status = "passed"
artifact_paths = ["target/ee-e2e/cass_robot_contract/<run-id>/"]
notes = "Validated capabilities, search, view, expand, and unknown schema handling."
```

## Future Validator

Suggested future command:

```bash
ee closure validate --dossier <path> --json
```

Suggested summary shape:

```json
{
  "schema": "ee.closure_dossier.validation.v1",
  "success": true,
  "data": {
    "beadId": "eidetic_engine_cli-hkk7",
    "profile": "docs_light",
    "requiredFieldsPresent": true,
    "vagueClaimsFound": [],
    "skippedChecksHaveRationale": true,
    "artifactReferencesValid": true
  },
  "degraded": []
}
```

The validator is owned by the future verification orchestration and artifact
policy beads. Until it exists, agents must include the dossier summary directly
in the bead close reason or final bead comment.
