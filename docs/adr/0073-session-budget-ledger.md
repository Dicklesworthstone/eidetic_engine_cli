# ADR 0073: Session Budget Ledger and Cost Planner Substrate

Status: proposed
Date: 2026-06-14
Bead: bd-1clqr.1 (epic bd-1clqr, 2026-06 idea-wizard wave)

## Context

Agents repeatedly spend scarce session budget rediscovering which command is
cheap enough for the current posture. Today a harness sees the cost after the
fact: output size, pack token use, RCH contention, Agent Mail degradation, DB
lock pressure, and derived-asset staleness are scattered across response
envelopes, stderr, queue probes, and coordination messages. That makes a
"cheapest useful next command" planner guess from static command names rather
than observed cost deltas.

The session-budget feature is deliberately smaller than a full telemetry
system. It records one opt-in, redaction-safe ledger row per observed command
or coordination pass. The row contains counts, hashes, source classifications,
and correlation identifiers only. It never stores raw command text, raw output,
memory bodies, mail bodies, private absolute paths, secrets, or prompt text.

## Decision

### 1. Normative row schema

`ee.session_budget.v1` is the ledger-row contract:
[`docs/schemas/ee.session_budget.v1.json`](../schemas/ee.session_budget.v1.json).
The schema is marked `x-ee-status.shipped = false` until bd-1clqr.2 lands the
recording path. The schema itself is the stable contract for later recording,
retention, and planner work.

Each row includes:

| Area | Fields |
|---|---|
| Identity | `eventId`, `recordedAt`, `workspaceFingerprint` |
| Opt-in | `optIn.enabled = true`, opt-in source, sampling rate |
| Correlation | session id, command id, parent command id, task hash, pack id, RCH job id, Agent Mail thread id, Beads id |
| Command class | normalized surface and command class, read-only flag, durable-mutation flag |
| Output cost | estimated/returned output tokens and output bytes |
| Pack cost | requested and used pack tokens |
| Time cost | wall-clock milliseconds |
| RCH cost | slots requested, slots used, blocked milliseconds, queue depth, healthy workers |
| DB cost | DB lock wait, read-pool acquire wait, write-attempt count |
| Derived-asset cost | freshness penalty and stale source categories |
| Degradation | grouped code/source/severity/count records |
| Privacy | redaction status plus const-false raw content storage flags |
| Retention | bounded rows, max age, evicted-row count |
| Evidence | bounded hash/provenance references, never raw content |

### 2. Privacy and redaction policy

The row is `paths_counts_hashes_no_content` by construction:

- `rawCommandStored`, `rawOutputStored`, and `contentStored` are all `false`.
- Commands are normalized to a small enum such as `ee recall`, `ee pack`, or
  `rch cargo verification`.
- Task text is represented by `taskHash`, not raw task content.
- Evidence refs are bounded identifiers and BLAKE3 hashes.
- Paths, when a later recorder needs them, must be workspace-relative or
  hashed; host-private absolute paths are not allowed in this schema.

### 3. Opt-in and zero-overhead disabled path

The recorder in bd-1clqr.2 must be explicitly enabled by CLI flag, environment,
or config. When disabled, it must avoid estimator, queue, ledger, and retention
work. This ADR does not introduce any always-on measurement path.

### 4. Retention

The ledger is bounded per workspace. The row carries the retention policy that
was active when it was recorded: `maxRowsPerWorkspace`, `maxAgeDays`, and
`evictedRows`. The recording path may evict old rows, but it must not silently
mutate memory records or derived search/graph assets.

### 5. Planner handoff

bd-1clqr.3 consumes rows as advisory evidence. A planner can recommend:
`primer`, `recall`, `search`, `pack`, `ask`, `swarm brief`, `work-packet`, or
`proof wait/skip`. It must never recommend local Cargo. RCH posture is recorded
only as remote-proof cost and blockage evidence.

## Examples

Four fixtures pin the initial examples:

- `tests/fixtures/session_budget/cheap_recall.json`
- `tests/fixtures/session_budget/large_pack.json`
- `tests/fixtures/session_budget/rch_blocked_proof.json`
- `tests/fixtures/session_budget/agent_mail_degraded_coordination.json`

They cover the acceptance dimensions from the bead: cheap recall, large pack,
RCH-blocked proof, and Agent Mail degraded coordination.

## Consequences

- Later recording work has a stable, redaction-safe target instead of inventing
  ad hoc telemetry rows.
- The planner can reason from observed cost deltas while preserving local-first
  and no-raw-content constraints.
- The schema deliberately does not make cost collection global or automatic.
  Disabled recording stays zero or near-zero overhead.

## Rejected Alternatives

- **Raw command logs:** rejected because they leak task text, paths, and
  sometimes secrets.
- **Always-on telemetry:** rejected because tight agent hooks cannot pay an
  estimator or write-path tax when the feature is unused.
- **Embedding rows in pack replay ledgers:** rejected because replay ledgers
  explain pack selection, while session budget rows explain cross-command cost.
- **Local Cargo recommendation fields:** rejected because this repo requires
  RCH-only Rust verification on the Mac swarm lane.

## Verification

- `tests/session_budget_schema_unit.rs` pins the schema identity, known-schema
  registration, required field sets, privacy consts, enum-backed examples, and
  deterministic fixture parse/serialize behavior.
- bd-1clqr.2 adds recorder unit tests proving the disabled path avoids ledger
  work, enabled recording writes bounded rows, and retention prevents unbounded
  growth.
- bd-1clqr.3 adds planner tests proving deterministic recommendations,
  explainable fallbacks, and local-Cargo refusal.
