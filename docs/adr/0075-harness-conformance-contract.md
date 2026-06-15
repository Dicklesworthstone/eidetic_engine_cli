# ADR 0075: Harness Conformance Contract

Status: accepted
Date: 2026-06-14
Bead: bd-i0iiw.1 (epic bd-i0iiw, 2026-06 idea-wizard wave)

## Context

`ee` already emits hook-install plans for agent harnesses, but the next
harness-conformance work needs a stable target before simulators and doctor
surfaces can land. Without a shared contract, each harness adapter can define
its own event names, fixture shape, pass/fail vocabulary, transcript policy,
and local-build behavior. That makes future additions expensive and makes it
too easy for a test harness to pass by accepting unbounded transcripts,
unredacted payloads, or forbidden local Cargo fallbacks.

`ee.harness_conformance.v1` is a redaction-safe fixture contract for testing
agent harness integration behavior. It started as a fixture and report shape;
bd-i0iiw.2, bd-i0iiw.3, and bd-i0iiw.4 then added the simulator, doctor/install
audit surface, and committed golden proof without renegotiating vocabulary.

## Decision

### 1. Schema and fixture unit

Normative schema:
[`docs/schemas/ee.harness_conformance.v1.json`](../schemas/ee.harness_conformance.v1.json).

One fixture describes one conformance case. A case records:

- `fixtureVersion`: major version `1`, with additive minor updates only.
- `caseId`, `harness`, `fixtureKind`, and `eventName`.
- `harnessSupport`: the local support matrix row for that harness.
- `input`: the redacted event shape, command template when present, and a
  bounded transcript excerpt.
- `expected`: the expected conformance verdict, event outcome, exit policy,
  degradation policy, output budget, and the permanent
  `localCargoFallbackAllowed = false` assertion.
- `assertions`: mechanical checks the simulator or doctor must evaluate.
- `artifactPolicy`: bounds for transcripts and generated artifacts.
- `compatibility`: schema-major pinning and fixture-version policy.

The schema carries `x-ee-status.shipped = true` after the simulator,
doctor/install audit surface, fixture provenance, documentation, and golden
proof are present. Future hook changes must update the fixtures and committed
goldens deliberately.

### 2. Supported harness matrix

The v1 harness ids are fixed:

| Harness id | Transport | Initial support | Event scope |
|---|---|---|---|
| `codex` | `hook_json` | supported | session, pre-tool, post-tool, compaction/resume |
| `claude-code` | `hook_json` | supported | session, pre-tool, post-tool, compaction/resume |
| `generic-shell` | `shell_env` | adapter | pre-tool shell and post-tool exit policy |
| `mcp-client` | `mcp_json_rpc` | adapter | tool-call result and degraded/error envelopes |

Adding a harness requires a new enum value, at least one fixture, and an ADR
amendment or successor. The enum is deliberate: misspelled harness ids should
fail schema-unit tests instead of silently creating a new fixture family.

### 3. Fixture taxonomy

The v1 fixture kinds are:

- `session_start`
- `pre_tool_edit`
- `pre_tool_shell`
- `post_tool_success`
- `post_tool_failure`
- `compaction_resume`

The v1 event names are the normalized names `SessionStart`, `PreToolUse`,
`PostToolUse`, and `CompactionResume`. Harness-specific payload names stay in
`input.payloadShape`; simulator code maps them to the normalized event name.

### 4. Pass/fail vocabulary

Conformance verdicts are:

- `pass`: the harness behavior satisfies every required assertion.
- `fail`: the harness behavior violated a required assertion.
- `blocked`: the fixture could not run because required infrastructure was
  unavailable, for example RCH admission is closed.
- `unsupported`: the harness is known but cannot support the fixture kind.

Assertion statuses are `pass`, `fail`, and `not_applicable`. Event outcomes
are separate: `success`, `failure`, and `not_applicable`. This split keeps a
PostToolUse failure fixture from being confused with a failing conformance
test; a harness can conform by handling a tool failure correctly.

### 5. Required assertions

The v1 assertion vocabulary is exactly:

- `command_invoked`
- `json_envelope_valid`
- `output_budget_respected`
- `degraded_handled`
- `secret_redaction`
- `non_zero_exit_policy`
- `no_local_cargo_fallback`

The simulator and doctor surfaces may add detail to assertion messages, but
they must not create ad hoc assertion names. The schema-unit test pins this
vocabulary so new assertion names require intentional contract work.

### 6. Redaction and artifact policy

Fixtures are not raw transcripts. They may contain only bounded, redacted
excerpts:

- `input.redactionStatus` is the const `redacted_bounded_no_secrets`.
- Transcript lines are limited to 256 bytes, and transcript excerpts to
  8192 bytes.
- `artifactPolicy.rawTranscriptAllowed` is the const `false`.
- `artifactPolicy.secretMaterialAllowed` is the const `false`.
- Artifact bytes are capped at 65536 bytes per fixture.
- Raw absolute private paths, bearer tokens, API keys, and private key blocks
  are not fixture content. Use redacted tokens such as `[REDACTED:secret]`.

### 7. Local Cargo policy

Every fixture carries `expected.localCargoFallbackAllowed = false`. A fixture
may include a redacted command template such as `cargo test --lib` to test
that the harness refuses local fallback, but the simulator must not execute
local Cargo. RCH-only verification remains a property of the surrounding
swarm workflow, not something conformance tests can waive.

## Consequences

- Future harness additions become mechanical: add an enum value, support-row
  fixture, and assertion coverage.
- Simulator output can stay compact because the fixture contract already
  defines redaction and artifact bounds.
- Doctor/install-audit surfaces can reuse the same pass/fail vocabulary
  instead of inventing new degraded codes for each harness.
- The contract is shipped as a fixture/simulator contract, not a raw transcript
  archive. Live schema registry changes remain separate and must not weaken the
  bounded redaction policy.

## Rejected Alternatives

- **Using raw harness transcripts as fixtures:** rejected because transcripts
  are large, unstable, and likely to contain private paths or secrets.
- **Allowing free-form harness ids:** rejected because typos and partial
  adapters would look like supported harnesses.
- **Conflating event failure with conformance failure:** rejected because
  PostToolUse failure handling is a required successful conformance case.
- **Leaving local Cargo fallback as a fixture option:** rejected permanently;
  fixtures can test the denial path but cannot permit local Cargo.

## Verification

- bd-i0iiw.1 (this bead): schema-unit test pins schema identity, required
  fields, harness/event/assertion vocabularies, redaction and budget constants,
  fixture compatibility policy, and round-trip validation for the v1 fixtures.
- bd-i0iiw.2: hook-event/transcript simulator consumes these fixtures.
- bd-i0iiw.3: doctor/install-audit surfaces report conformance status using
  the same vocabulary.
- bd-i0iiw.4: committed golden summaries pin real simulator output over every
  fixture, `tests/fixtures/harness_conformance/PROVENANCE.md` documents fixture
  origin and update policy, and
  `docs/agent-ux/harness-conformance.md` documents RCH-only proof before hooks
  are enabled in a real harness.
