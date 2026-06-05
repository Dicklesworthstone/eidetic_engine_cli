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

## Redaction

Proof broker rows must not carry raw stdout, stderr, mail bodies, memory
bodies, environment dumps, secrets, or host-private paths. They may carry
content hashes, run ids, redacted artifact refs, bounded owner metadata, Beads
ids, Agent Mail thread ids, and build-slot labels.

`rawOutputIncluded` is always `false`. Each evidence ref sets `redacted: true`
to document that it is safe for support bundles and handoffs.

## Non-goals

- No Cargo, RCH, Beads, Agent Mail, Git, or tracker mutation happens when this
  schema is constructed.
- This schema does not replace `ee.verification.run.v1` or
  `ee.verification.broker_view.v1`.
- It does not make `ee` a scheduler, autonomous agent loop, or build farm.
- It does not permit local Cargo fallback for remote-required proof.
