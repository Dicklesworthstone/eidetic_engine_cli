# Proof Broker Fingerprint And Ledger

Schema: `ee.proof_broker.v1`

Tracking bead: `bd-1n3x1.1`

This schema defines the redaction-safe proof broker record used to decide
whether a proof request may reuse existing evidence, wait on an in-flight
owner, dispatch one remote proof, or reject the request as stale or unsafe.
It complements `ee.verification.broker_view.v1`: the existing broker view is
an operator projection over retained runs, while this schema captures the
canonical request fingerprint and ledger row that later admission and RCH
integration surfaces will consume.

## Fingerprint Boundary

The fingerprint ID includes every field that can make proof reuse unsafe:
bead id, command class, command hash, normalized argv hash, source tree
fingerprint, source materialization mode, dirty status hash, environment
fingerprint class, target profile, execution substrate, RCH runtime class,
worker requirement, local Cargo tripwire class, and build-admission posture.

Missing evidence is represented as explicit `class:*` values such as
`class:unknown_source`, `class:unknown_env`, or `class:tripwire_unknown`.
Agents must treat those classes as admission constraints, not as proof that the
source or environment is equivalent.

## Admission Verdicts

| Verdict | Meaning | Next action pattern |
| --- | --- | --- |
| `reuse_existing` | A completed proof row matches the current fingerprint and remains fresh. | Cite the matched run id and evidence refs. |
| `wait_for_inflight` | An equivalent proof is already owned by another agent or job. | Wait, watch, or coordinate with the owner. |
| `dispatch_allowed` | No equivalent proof row exists and the request is safe to run once. | Launch one RCH proof lane. |
| `source_state_mismatch` | Command evidence exists, but source materialization or dirty state differs. | Rerun against current source. |
| `environment_blocked` | RCH/runtime/worker posture prevents authoritative proof. | Repair or wait for the environment. |
| `proof_unusable` | Evidence exists but violates policy, such as local Cargo bypass. | Discard that evidence for closeout. |
| `unknown_insufficient_evidence` | The broker lacks enough source, env, or tripwire data to decide. | Collect missing evidence before dispatch. |

## Conformance Matrix

The proof-broker conformance contract is intentionally split between
Cargo-free public-surface checks and RCH-only source proof. Static/public
coverage can be refreshed without launching Cargo; source closeout still needs
an RCH verdict from the command recipe below.

| Requirement | Level | Static/public harness | RCH/live harness | Status |
| --- | --- | --- | --- | --- |
| Completed equivalent proof returns `reuse_existing` and does not dispatch Cargo. | MUST | `scripts/e2e_overhaul/proof_broker_admission.sh` case `reuse_existing_admit`; `tests/rch_verify_contract.rs` `proof_broker_reuse_existing_skips_remote_dispatch` | RCH recipe for `tests/rch_verify_contract.rs` | Static covered; live RCH pending. |
| Equivalent in-flight proof returns `wait_for_inflight` with owner/job metadata and no duplicate dispatch. | MUST | `wait_for_inflight_admit`; `proof_broker_wait_for_inflight_refuses_before_remote_dispatch` | RCH recipe for `tests/rch_verify_contract.rs` | Static covered; live RCH pending. |
| No equivalent proof returns `dispatch_allowed` and launches exactly one remote proof lane. | MUST | `dispatch_allowed_admit`; `proof_broker_dispatch_allowed_launches_single_remote_proof` with fake RCH invocation log | RCH recipe for `tests/rch_verify_contract.rs` | Static covered; live RCH pending. |
| Stale or dirty source evidence returns `source_state_mismatch`. | MUST | `source_state_mismatch_admit`; `proof_broker_source_mismatch_refuses_before_remote_dispatch` | RCH recipe for `tests/rch_verify_contract.rs` | Static covered; live RCH pending. |
| RCH/runtime/worker blockers return `environment_blocked` and do not dispatch. | MUST | `environment_blocked_admit`; `proof_broker_environment_blocked_refuses_before_remote_dispatch` | RCH recipe for `tests/rch_verify_contract.rs` | Static covered; live RCH pending. |
| Local Cargo bypass evidence returns `proof_unusable` and is not reusable for closeout. | MUST | `proof_unusable_admit`; `proof_broker_local_cargo_bypass_is_unusable_without_bypass` | RCH recipe for `tests/rch_verify_contract.rs` | Static covered; live RCH pending. |
| Explicit operator bypass records the reason and still marks the broker verdict degradation. | SHOULD | `proof_broker_explicit_bypass_runs_remote_and_records_reason` with fake RCH output | RCH recipe for `tests/rch_verify_contract.rs` | Static covered; live RCH pending. |
| Insufficient source/env/tripwire evidence returns `unknown_insufficient_evidence`. | MUST | `unknown_insufficient_evidence_admit`; golden ledger coverage in `tests/verification_evidence_schema_unit.rs` | RCH recipe for `tests/rch_verify_contract.rs` | Static covered; live RCH pending. |
| Public event logs include command, cwd/workspace, sanitized env, elapsed time, exit code, artifact paths, schema/redaction status, broker verdict, fingerprint, reuse id/hash, and first-failure diagnosis. | SHOULD | `scripts/e2e_overhaul/proof_broker_admission.sh` `emit_broker_event` and `proof_broker_custom_event_rows` | RCH recipe for `tests/rch_verify_contract.rs` plus retained `ee.rch.verify.v1` artifacts | Static covered; live RCH pending. |

## Redaction

Proof broker rows must not carry raw stdout, stderr, mail bodies, memory
bodies, environment dumps, secrets, or host-private paths. They may carry
content hashes, run ids, redacted artifact refs, bounded owner metadata, Beads
ids, Agent Mail thread ids, and build-slot labels.

`rawOutputIncluded` is always `false`. Each evidence ref sets `redacted: true`
to document that it is safe for support bundles and handoffs.

## Owner Bridge

`ee proof admit --json` and `ee proof status --json` expose two additive fields
for coordination:

- `ownerStatus` is a compact, redaction-safe summary of the matched ledger row's
  owner. It can be embedded in support bundles and handoff capsules without raw
  Agent Mail bodies. The field includes `status`, `active`, `shouldWait`,
  `owner`, `expiresAt`, `reasonCodes`, and `recoveryActions`.
- `coordination` records caller-supplied live coordination posture such as
  `--agent-mail-status unavailable`, `disabled`, `reservation_conflict`, or
  `owner_gone`. Admission remains local and read-only; live Agent Mail
  unavailability becomes a coordination degradation, not a proof blocker.

In-flight owner expiry is deterministic. Pass `--now <RFC3339>` in tests,
support-bundle generation, or handoff rendering so `ownerStatus.status` and the
admission verdict are reproducible. When an equivalent in-flight row has
expired, admission changes from `wait_for_inflight` to `dispatch_allowed` with
`owner_expired`, and the next command tells the agent to dispatch one fresh
remote proof or refresh the owner. No cleanup, lease release, Beads mutation, or
Agent Mail write is required.

Agent Mail build slots are optional metadata. If a live build-slot API is
available, callers can include the slot label in `owner.buildSlot`; if build
slots are disabled or Agent Mail is unavailable, the broker still uses the
ledger row, RCH job id, expiry, and evidence refs. Agents should coordinate
through the listed `owner.mailThreadId`, `owner.beadId`, and `owner.rchJobId`
when they are present, but should not launch a duplicate proof while
`ownerStatus.shouldWait` is `true`.

## Support Bundles And Handoffs

Support bundles include `proof_broker_summary.json` when generated by an ee
version with the proof-broker capsule bridge. The summary is a redaction-safe
projection over retained proof-broker ledger rows, not a raw ledger copy. It
includes fingerprint ids, verdict/state counts, source-materialization class,
local Cargo tripwire class, RCH runtime class, owner refs, evidence ref ids and
hashes, stale/redaction degraded codes, and next-command posture. It excludes
raw commands, stdout/stderr, Agent Mail bodies, memory bodies, environment
dumps, secrets, and host-private paths.

The default ledger location for swarm handoffs is:

```bash
.ee/derived/rch/proof_broker_ledger.json
```

When using `scripts/rch_verify.sh` with proof admission, pass that path unless a
bead or local runbook says otherwise:

```bash
scripts/rch_verify.sh \
  --proof-broker-ledger .ee/derived/rch/proof_broker_ledger.json \
  -- cargo test --workspace --lib proof_
```

`ee handoff preview`, `ee handoff create`, and `ee handoff resume` embed the
same support summary as a `proof_broker_summary` section. Treat it as
diagnostic continuity only. Before reusing, waiting on, or dispatching a proof,
refresh live admission:

```bash
ee proof admit --json \
  --ledger-json .ee/derived/rch/proof_broker_ledger.json \
  -- cargo test --workspace --lib <filter>
```

Use this Beads/Agent Mail citation shape:

```text
proof_broker_summary=<summaryHash> verdict_counts=<admissionCounts> stale_records=<n>; refreshed with ee proof admit before closeout.
```

Do not paste raw RCH logs, raw mail bodies, or full command lines into Beads
comments just because a proof-broker summary exists. Cite the summary hash,
run id, owner refs, and focused verification commands instead.

Example active owner status:

```json
{
  "status": "active",
  "active": true,
  "shouldWait": true,
  "source": "proof_broker_ledger",
  "agentMailStatus": "fresh",
  "agentMailRequired": false,
  "owner": {
    "agentName": "RubyWolf",
    "beadId": "bd-1n3x1.1",
    "mailThreadId": "8198",
    "buildSlot": "proof:bd-1n3x1.1:broker",
    "rchJobId": "rch-job-20260605-0001"
  },
  "expiresAt": "2026-06-05T18:21:00Z",
  "reasonCodes": ["equivalent_inflight", "owner_active"],
  "recoveryActions": ["wait_for_owner_or_watch_job"]
}
```

Example expired owner status:

```json
{
  "status": "expired",
  "active": false,
  "shouldWait": false,
  "source": "proof_broker_ledger",
  "agentMailStatus": "unavailable",
  "agentMailRequired": false,
  "owner": {
    "agentName": "RubyWolf",
    "beadId": "bd-1n3x1.1",
    "mailThreadId": "8198",
    "buildSlot": "proof:bd-1n3x1.1:broker",
    "rchJobId": "rch-job-20260605-0001"
  },
  "expiresAt": "2026-06-05T18:21:00Z",
  "reasonCodes": ["equivalent_inflight_expired", "owner_expired"],
  "recoveryActions": ["dispatch_fresh_proof_or_refresh_owner"]
}
```

## Non-goals

- No Cargo, RCH, Beads, Agent Mail, Git, or tracker mutation happens when this
  schema is constructed.
- This schema does not replace `ee.verification.run.v1` or
  `ee.verification.broker_view.v1`.
- It does not make `ee` a scheduler, autonomous agent loop, or build farm.
- It does not permit local Cargo fallback for remote-required proof.
