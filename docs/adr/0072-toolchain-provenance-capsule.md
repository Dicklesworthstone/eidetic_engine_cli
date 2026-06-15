# ADR 0072: Toolchain Provenance Capsule

Status: proposed
Date: 2026-06-10
Bead: bd-aunn3.1 (epic bd-aunn3, 2026-06 idea-wizard wave)

## Context

Swarm agents in this repo depend on a chain of coordination binaries and
scripts — `ee` itself, `rch`, `br`, `bv`, the Agent Mail server, and the
`scripts/` helpers — and several incidents trace to *toolchain* staleness
rather than workspace state: an installed `ee 0.5.0` rejecting
`--claim-gate` flags the source contract documents, `bv --robot-next`
recommending a blocked bead, a corrupt Agent Mail store wedging pre-commit
hooks, and RCH wrappers silently absent. Today each surface
(claim gate, swarm brief, environment attestation, support bundle) probes a
subset of these tools ad hoc, and none of them can answer "*which* tool
versions and script hashes produced this evidence?" after the fact.

`ee.toolchain_provenance.v1` is a small, redaction-safe capsule that records
the observed toolchain at evidence-collection time. It complements — and
deliberately does not duplicate:

- **bd-3utv2 (installed-`ee` freshness):** owns the *decision* about whether
  the installed `ee` is authoritative for claim gating. This capsule only
  records the observation (`resolvedPath`, `version`, `binaryHash`,
  freshness verdict) that the bd-3utv2 logic consumes.
- **RCH-helper beads (bd-1n3x1.13/.14, bd-b1e4v):** own RCH admission,
  pressure telemetry, and topology-regression *blockers*. This capsule
  records only the `rch` binary/wrapper identity and probe outcome class.
- **ADR 0058-era environment attestation:** owns per-source *authority*
  verdicts. The capsule is one more redaction-safe evidence block such
  surfaces can embed, not a new authority calculus.

## Decision

### 1. Capsule shape: `ee.toolchain_provenance.v1`

One capsule = one collection pass over a fixed tool inventory, emitted under
the standard envelope by whichever surface embeds it (attestation, support
bundle, work-packet, doctor). Normative JSON Schema:
[`docs/schemas/ee.toolchain_provenance.v1.json`](../schemas/ee.toolchain_provenance.v1.json)
(`x-ee-status.shipped = false` until bd-aunn3.2 lands the collectors).

Top level: `schema`, `collectedAt`, `workspaceFingerprint` (the existing
12-hex workspace fingerprint, never the raw path), `redactionStatus`
(const `paths_workspace_relative_or_hashed_no_content`), `tools[]`,
`scriptHashes[]`, `degraded[]`.

### 2. Tool rows

Each `tools[]` row:

| Field | Meaning |
|---|---|
| `tool` | Stable id: `ee`, `rch`, `br`, `bv`, `agent_mail`, `cass`, `git`, `cargo` |
| `kind` | `binary`, `service`, or `script_suite` |
| `resolvedPath` | Workspace-relative when inside the workspace; otherwise `hashed:<blake3-12>` of the absolute path. Raw absolute private paths never appear |
| `version` | Tool-reported version string, or `null` with `freshness = version_unknown` |
| `binaryHash` | `blake3:<64-hex>` of the resolved binary; `null` for `service` rows |
| `sourceHint` | Bounded provenance hint: `release_install`, `cargo_target`, `system_package`, `unknown` |
| `freshness` | One state from §3 |
| `degraded[]` | Bounded per-tool degradation codes (same vocabulary as §3 plus probe codes) |
| `checkedAt` | RFC 3339 collection timestamp |

Sources per tool are fixed by this ADR (bd-aunn3.2 implements them):
`which -a` resolution order, `ee install check`, `rch status`,
`br --version`, `bv --version`, the Agent Mail `/health` JSON or an explicit
`ee.agent_mail.snapshot.v1` file, and BLAKE3 hashes of the `scripts/`
helpers named in AGENTS.md (`scriptHashes[]` rows: `script`, `blake3`,
`tracked` flag).

### 3. Freshness state vocabulary

`current`, `stale_binary` (resolved binary older than the source/docs
contract requires, e.g. installed `ee` rejecting documented flags),
`source_mismatch` (binary hash does not match any known release or local
build artifact), `wrapper_missing` (expected wrapper/hook absent, e.g. RCH
cargo wrapper), `health_corrupt` (service reports a corrupt/recovery
store — the Agent Mail case), `command_timeout` (probe exceeded its bounded
timeout — the BV case), `version_unknown`, `unsupported_platform`.

States are observations, not policies. Mapping states to claim/verdict
consequences stays in the consuming surfaces (bd-aunn3.3).

### 4. Redaction policy

- Paths: workspace-relative or `hashed:` form only (§2). No home
  directories, no host-private absolute paths.
- No raw command stdout/stderr is embedded; probe outcomes are reduced to
  the state vocabulary plus bounded degradation codes.
- No Agent Mail message bodies, subjects, or sender names; the
  `agent_mail` row carries only health-level fields already exposed by the
  health endpoint (`status`, `recovery.mode`).
- Version strings and hashes are considered safe; they are the point.

### 5. Claim-gate semantics (fail-closed, one-directional)

A capsule can only *remove* authority, never add it:

- `health_corrupt` or `stale_binary` on a critical tool (`ee`, `br`,
  `agent_mail`) makes the corresponding source authority `false` in
  consumers that adopt the capsule.
- A fully `current` capsule is **never** sufficient evidence to claim:
  reservations, tracker authority, and the work-packet gate still decide.
- Absent or unparseable capsule ⇒ consumers behave exactly as today
  (no new failure mode introduced by adoption).

### 6. Degradation codes

Capsule-level `degraded[]` entries use the standard
`{code, severity, message, repair}` shape with codes
`toolchain_probe_timeout` (low), `toolchain_tool_unresolved` (low), and
`toolchain_hash_unavailable` (info). Per-tool rows reuse the §3 states as
codes where a row-level explanation is needed. Rows land in
`docs/degraded_code_taxonomy.md` with the emitting commit (bd-aunn3.2),
per the same-commit rule.

### 7. Agent citation guidance

Agents should cite toolchain evidence as a compact provenance summary, not as
raw capsule JSON. In Beads comments, closeouts, and Agent Mail coordination,
include:

- the surface that produced the capsule, for example
  `ee diag toolchain-provenance --workspace . --json`, a support-bundle
  `toolchain_provenance.json`, or a swarm work-packet source-authority block;
- `schema`, `collectedAt`, `workspaceFingerprint`, and `redactionStatus`;
- the relevant tool rows by stable `tool` id, `freshness`, probe
  `exitClass`, and degraded `code` values;
- script evidence by workspace-relative `script` and BLAKE3 hash or short
  hash preview when the exact hash is too noisy for the thread;
- the exact RCH or claim-gate blocker string when a proof or claim was
  stopped before source tests.

Do not paste raw command stdout/stderr, home-directory paths, Agent Mail
message metadata, or full support-bundle payloads into tracker comments or
mail. A green capsule is still only supporting evidence: cite it alongside
tracker state, live reservations, and RCH proof status; never present it as
sufficient authority to claim or close work by itself.

## Consequences

- Support bundles and attestations gain a deterministic answer to "what
  toolchain produced this evidence?", which is the recurring gap in
  stale-binary and corrupt-mail incidents.
- One more capsule to keep redaction-honest; the schema test pins the
  redaction-relevant constants so drift fails CI before it ships.
- Probes are bounded and read-only by construction; the capsule collector
  (bd-aunn3.2) may not mutate Beads, Agent Mail, RCH state, or git.

## Rejected Alternatives

- **Folding tool rows into `ee.environment_attestation`:** attestation is
  about source *authority*; mixing observation rows into it would force a
  schema major-version bump and duplicate bd-3utv2's decision logic.
- **Recording raw `which`/`--version` output:** unbounded, redaction-unsafe,
  and host-revealing. The fixed row shape is the contract.
- **Letting a green capsule unlock claims:** rejected outright; §5 keeps the
  capsule one-directional so it can never make unsafe work look safe.

## Verification

- bd-aunn3.1 (this bead): schema-unit test pinning `$id`/title/const
  agreement, the required-field sets, the §3 state enum, the redaction
  const, and the four round-trip fixtures (`fresh`, `stale_binary`,
  `agent_mail_corrupt`, `bv_rch_timeout`) validating structurally against
  the schema.
- bd-aunn3.2: read-only collectors + taxonomy rows + no-mutation proof.
- bd-aunn3.3: claim-gate/support-bundle integration behind the §5 rule.
- bd-aunn3.4: conformance matrix, no-mock smoke, docs, RCH-only proof.
