# ADR 0026: Deterministic Pack Quality Sentinel

Status: proposed
Date: 2026-05-09

## Context

ADR 0007 makes context packs the primary `ee` user experience. ADR 0025 adds
replayable context pack selection ledgers so agents can explain and diff a
previously emitted pack. The codebase also already has per-pack quality
metrics, deterministic eval fixtures, golden JSON contracts, redaction egress
tests, pack replay and diff commands, evidence freshness reporting, and
performance regression gates.

Those pieces answer important local questions:

1. Is this one pack internally well formed?
2. Can this historical pack be replayed or diffed?
3. Did a command stay within a latency or memory budget?
4. Did this fixture command succeed and render a stable shape?

They do not yet answer a behavioral drift question that matters to agents:

> For a canonical task, did the context pack select the evidence we expect,
> omit the evidence we expect, preserve provenance and redaction posture, and
> stay useful under degraded derived assets?

Without that sentinel, a retrieval or packing change can keep JSON schemas,
latency budgets, and per-pack quality metrics green while silently dropping the
one memory that matters for a release, cleanup, migration, privacy, or
multi-workspace task. The existing eval fixtures and pack ledgers are the right
raw material for detecting that drift, but `ee` needs a deterministic contract
for turning them into a CI-safe and agent-readable signal.

The sentinel must respect existing constraints:

- ADR 0001 keeps `ee` CLI-first, local-first, and harness-agnostic.
- ADR 0002 keeps FrankenSQLite and SQLModel as the source of truth.
- ADR 0004 delegates retrieval to Frankensearch.
- ADR 0007 keeps context packs as the primary UX.
- ADR 0008 treats graph metrics as derived features.
- ADR 0009 defines trust and prompt-injection posture.
- ADR 0011 keeps command surfaces mechanical rather than agent-reasoning loops.
- ADR 0013 requires durable writes to go through the single write owner.
- ADR 0024 keeps performance forensics read-only over existing artifacts.
- ADR 0025 defines replay ledgers as redaction-safe pack evidence, not a second
  memory store.

## Decision

`ee` will add a deterministic pack quality sentinel under the eval command
family. The sentinel runs fixture-backed context or pack requests through the
real selection path, reads the persisted pack ledger, and compares the actual
pack behavior against explicit fixture expectations.

The preferred public shape is:

```text
ee eval run --pack-quality --json [--scenario <id>] [--fixture-dir <path>]
```

An implementation may choose a different eval-namespaced spelling if it better
matches the current CLI parser, but the semantics are fixed:

- The command is read-only with respect to EE durable state.
- It does not require daemon mode, network access, cloud models, or paid APIs.
- It does not implement a second retrieval stack.
- It does not call an LLM judge.
- It does not mutate memories, indexes, graph snapshots, pack records, Beads,
  support bundles, or config.
- JSON stdout is the stable machine contract. Human progress, repair text, and
  diagnostics remain on stderr or in explicit artifacts.

The report schema is `ee.eval.pack_quality_report.v1`. It compares fixture
expectations to observed behavior and includes:

- Fixture directory, scenario ID, fixture hash, and source memory fixture hash.
- Command surface, query text or query-file digest, profile, format, token
  budget, and effective speed mode.
- Workspace ID, database generation, index generation, graph snapshot
  reference, cache/profile evidence references when present, and derived-asset
  degradation posture.
- Pack ID, pack hash, ledger hash, ledger schema, and ledger status.
- Expected and actual selected memory IDs in stable order.
- Expected and actual omitted critical memory IDs.
- Rank, section, score, trust class, redaction class, and `why` hash deltas
  when the ledger exposes them.
- Provenance density, minimum required provenance density, and missing
  provenance references.
- Expected and actual degradation codes, including additions and removals.
- Expected and actual redaction posture, including forbidden leak classes.
- Pass, fail, or degraded status for each scenario and for the aggregate run.
- First-failure diagnosis with exact reproduction commands.

The sentinel is a behavioral contract over existing systems. It may orchestrate
existing context, pack, eval, fixture, redaction, and ledger services, but it
must not duplicate their core algorithms.

### Fixture Expectations

Pack-quality expectations are additive to deterministic eval fixtures. The
fixture schema should allow either an embedded section or a versioned companion
fixture. The initial expectation model includes:

- `query` or `query_file` input.
- `profile`, `max_tokens`, `format`, and speed mode expectations.
- `must_select_memory_ids`: memory IDs that must appear in the pack.
- `must_omit_memory_ids`: memory IDs that must not appear in the pack.
- `critical_memory_ids`: memory IDs whose omission is a hard failure.
- `allowed_extra_memory_ids`: optional IDs that may appear without failing the
  scenario.
- `expected_degradation_codes` and `forbidden_degradation_codes`.
- `min_provenance_density`.
- `forbidden_redaction_leak_classes`.
- `rank_tolerance` for memories whose exact rank is not semantically important.
- `derived_asset_posture` expectations such as lexical-only, graph-unavailable,
  stale index, or fresh baseline.

Fixture validation must reject unknown memory IDs, duplicate IDs, impossible
token budgets, invalid degradation codes, unsupported rank tolerances, and
ambiguous scenario references.

### Status And Exit Semantics

The command reports scenario-level and aggregate status:

- `passed`: expectations matched.
- `failed`: expectations did not match.
- `degraded`: the command produced an honest partial answer under a declared
  degraded posture.
- `inconclusive`: required evidence was unavailable, malformed, or outside the
  fixture contract.

Successful execution with failing pack-quality assertions is still a machine
readable evaluation failure, not a panic or ad hoc usage error. Exit-code
mapping follows the project table:

- Usage and malformed fixture selection use code 1.
- Configuration and fixture-directory failures use code 2.
- Storage and ledger-read failures use code 3.
- Search, index, and derived-asset failures that prevent the requested mode use
  code 4 or 6 depending on whether degraded operation was permitted.
- Policy denial or redaction fail-closed cases use code 7.
- A completed evaluation with failed expectations uses the existing eval
  command convention for fixture failure and must remain stable once wired.

### Determinism

The same database, fixtures, query inputs, derived-asset manifests, and `ee`
version must produce the same report. Stable ordering applies to:

- scenarios by fixture ID and scenario ID,
- memory IDs by expected order and then memory ID,
- degradation records by severity and code,
- redaction classes lexicographically,
- failure diagnoses by scenario ID, severity, and stable code,
- reproduction commands by command surface and scenario ID.

The report may include elapsed time only if existing eval output already treats
timing as advisory and not part of hash-critical output. Any deterministic hash
must exclude nondeterministic wall-clock fields.

## Consequences

The sentinel gives agents a sharper regression signal than schema, latency, or
single-pack metrics alone. A future change that keeps JSON valid but drops the
release rule from the release pack can fail a deterministic fixture before it
reaches users.

It also makes degraded retrieval easier to reason about. Fixtures can say that a
lexical-only branch is acceptable if it still selects the critical memory, or
that a stale graph snapshot is acceptable only if the report surfaces the right
degradation and repair hint.

The design adds implementation obligations:

- Eval fixture schemas need additive pack-quality expectations and validation.
- The evaluator needs a thin orchestration layer over existing context, pack,
  ledger, redaction, and eval services.
- Golden and schema tests must pin the report before agents rely on it.
- No-mock E2E must prove the command uses real binaries, real local workspaces,
  real persisted ledgers, and redaction-safe artifacts.
- Documentation must keep pack-quality sentinel reports distinct from per-pack
  quality metrics, pack diff, performance benchmarks, and support bundles.

The sentinel intentionally does not solve all retrieval quality problems. It is
a deterministic guardrail over curated fixtures, not an open-ended relevance
judge. Expanding the fixture corpus is how the quality signal improves.

## Rejected Alternatives

- **Use an LLM judge to grade pack quality.** That would add nondeterminism,
  cost, privacy risk, and hidden rubric drift. Human-authored fixtures are the
  source of expected behavior.
- **Create a separate retrieval evaluator.** A second evaluator would drift
  from the real context and pack paths. The sentinel must exercise the real
  selection code or the internal service layer that the real commands use.
- **Treat per-pack quality metrics as sufficient.** Metrics like provenance
  density and token utilization are useful but cannot know which specific
  memory a canonical task required.
- **Fold this into performance benchmarks.** Performance gates answer time and
  resource questions. Pack-quality fixtures answer behavioral selection
  questions.
- **Make support bundles the primary interface.** Support bundles are diagnostic
  packaging. The sentinel is an eval command that can later contribute safe
  summaries to support bundles.
- **Mutate memories or derived assets during evaluation.** A quality sentinel
  should not repair, rebuild, promote, demote, tombstone, or rewrite state while
  measuring behavior.
- **Fail silently when ledgers are missing.** Missing or malformed ledgers must
  produce stable inconclusive or degraded outcomes with repair commands.

## Verification

The decision remains true when the `eidetic_engine_cli-jhnj` track proves all
of the following:

1. `eidetic_engine_cli-rq4f` lands this ADR before implementation work starts.
2. `eidetic_engine_cli-fd3k` extends fixture contracts with pack-quality
   expectations and validation for valid, malformed, lexical-only, stale
   derived-asset, redaction, and query-file cases.
3. `eidetic_engine_cli-aqfn` implements the read-only comparison layer over real
   context or pack outputs and persisted ledgers, with unit tests for selected
   ID drift, omitted critical memory, degradation drift, redaction drift,
   missing ledger, malformed fixture, and empty workspace.
4. `eidetic_engine_cli-kag1` exposes the eval-namespaced CLI surface with stable
   `ee.response.v1` JSON stdout, stderr-only diagnostics, effect-manifest
   read-only classification, and existing exit-code conventions.
5. `eidetic_engine_cli-ayow` registers `ee.eval.pack_quality_report.v1`, adds
   golden fixtures for pass, fail, degraded, inconclusive, lexical-only,
   stale-derived-asset, missing-ledger, redaction-leak, and query-file cases,
   and verifies deterministic ordering.
6. `eidetic_engine_cli-mccc` adds logged no-mock E2E scenarios using real `ee`
   binaries and isolated workspaces. The dossier records command, cwd,
   sanitized environment, elapsed time, exit code, stdout/stderr artifact paths,
   fixture IDs, scenario IDs, selected/rejected memory IDs, pack IDs, pack
   hashes, ledger hashes, schema/golden validation, redaction status,
   degradation codes, and first-failure diagnosis.
7. `eidetic_engine_cli-2w6k` documents how to author and interpret sentinel
   fixtures after the current pack replay support-bundle documentation track is
   closed, so terminology and artifact fields stay aligned.
8. `br dep cycles --json` remains empty for the planning track.
9. Forbidden-dependency audits continue to reject Tokio, rusqlite, petgraph,
   and other banned crates.
